use crate::client::MysqlClient;
use crate::config::{Binding, Config, Instance, WorkflowRecord};
use crate::error::{AppError, Result};
use crate::ini;
use crate::lifecycle::{Lifecycle, Status};
use crate::process::Runner;
use crate::sql;

/// Shared context for every command: the loaded configuration and a runner.
pub struct Ctx<'a> {
    pub config: Config,
    pub runner: &'a dyn Runner,
}

impl<'a> Ctx<'a> {
    pub fn new(config: Config, runner: &'a dyn Runner) -> Self {
        Self { config, runner }
    }

    pub fn save(&self) -> Result<()> {
        crate::config::save(&self.config)
    }

    /// Loads `my.ini` for an instance, turning a missing file into a clear
    /// error that names the expected path.
    pub fn settings_for(
        &self,
        instance: &Instance,
    ) -> Result<(ini::MysqldSettings, std::path::PathBuf)> {
        let ini_path = instance.my_ini_path();
        if !ini_path.exists() {
            return Err(AppError::instance(format!(
                "配置文件 {} 不存在；登记实例时请用 `--my-ini` 指定正确的文件",
                ini_path.display()
            )));
        }
        let settings = ini::load(&ini_path)?;
        Ok((settings, ini_path))
    }

    pub fn client<'b>(
        &'b self,
        instance: &'b Instance,
        settings: &'b ini::MysqldSettings,
        ini_path: &'b std::path::Path,
    ) -> MysqlClient<'b> {
        MysqlClient::new(self.runner, instance, settings, ini_path)
    }

    pub fn lifecycle(&self) -> Lifecycle<'_> {
        Lifecycle::new(self.runner)
    }
}

pub fn add_instance(
    ctx: &mut Ctx<'_>,
    name: &str,
    home: &str,
    my_ini: Option<&str>,
    root_password: Option<&str>,
) -> Result<String> {
    sql::validate_identifier("instance", name)?;

    if ctx.config.find(name).is_some() {
        return Err(AppError::usage(format!(
            "实例 `{name}` 已登记；如需重新登记，请先运行 `mm instance remove {name}`"
        )));
    }

    let home_path = std::path::PathBuf::from(home);
    if !home_path.is_dir() {
        return Err(AppError::instance(format!(
            "安装目录不存在：{}",
            home_path.display()
        )));
    }
    let mysqld = home_path.join("bin").join("mysqld.exe");
    if !mysqld.is_file() {
        return Err(AppError::instance(format!(
            "{} 不是有效的 MySQL 安装：未找到 {}",
            home_path.display(),
            mysqld.display()
        )));
    }

    let instance = Instance {
        name: name.to_string(),
        home: home_path.display().to_string(),
        my_ini: my_ini.map(|value| value.to_string()),
        root_password: root_password.unwrap_or("").to_string(),
        last_pid: None,
    };

    let ini_path = instance.my_ini_path();
    if !ini_path.is_file() {
        return Err(AppError::instance(format!(
            "配置文件 {} 不存在；请用 `--my-ini` 指向正确的文件",
            ini_path.display()
        )));
    }
    // Fail early if the option file lacks the keys every command needs.
    let settings = ini::load(&ini_path)?;
    let datadir = settings.require_datadir(&ini_path)?;
    let port = settings.require_port(&ini_path)?;

    let detected = ctx
        .client(&instance, &settings, &ini_path)
        .detect_version()
        .ok();

    ctx.config.instances.push(instance);
    ctx.save()?;

    let version = detected
        .map(|value| format!("{}.{}.{}", value.major, value.minor, value.patch))
        .unwrap_or_else(|| "unknown".to_string());
    Ok(format!(
        "已登记实例 `{name}`\n  版本：  {version}\n  数据目录：{}\n  端口：  {port}\n  配置文件：{}",
        datadir.display(),
        ini_path.display()
    ))
}

pub fn remove_instance(ctx: &mut Ctx<'_>, name: &str) -> Result<String> {
    let index = ctx
        .config
        .instances
        .iter()
        .position(|item| item.name == name)
        .ok_or_else(|| AppError::instance(format!("实例 `{name}` 未登记")))?;

    let removed_bindings = ctx
        .config
        .bindings
        .iter()
        .filter(|item| item.instance == name)
        .count();
    ctx.config.instances.remove(index);
    ctx.config.bindings.retain(|item| item.instance != name);
    ctx.save()?;

    let mut message = format!("已从配置中移除实例 `{name}`（未改动任何文件）");
    if removed_bindings > 0 {
        message.push_str(&format!(
            "\n同时清除了该实例的 {removed_bindings} 条绑定记录"
        ));
    }
    Ok(message)
}

pub struct InstanceRow {
    pub name: String,
    pub version: String,
    pub port: String,
    pub status: String,
    pub datadir: String,
}

pub fn list_instances(ctx: &Ctx<'_>) -> Result<Vec<InstanceRow>> {
    let mut rows = Vec::new();
    for instance in &ctx.config.instances {
        let (version, port, status, datadir) = match ctx.settings_for(instance) {
            Ok((settings, ini_path)) => {
                let version = ctx
                    .client(instance, &settings, &ini_path)
                    .detect_version()
                    .map(|value| format!("{}.{}.{}", value.major, value.minor, value.patch))
                    .unwrap_or_else(|_| "未知".to_string());
                let status = match ctx.lifecycle().status(instance, &settings, &ini_path) {
                    Ok(Status::Running) => "运行中",
                    Ok(Status::Stopped) => "已停止",
                    Err(_) => "未知",
                };
                (
                    version,
                    settings
                        .port
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "-".to_string()),
                    status.to_string(),
                    settings.datadir.unwrap_or_else(|| "-".to_string()),
                )
            }
            Err(_) => (
                "未知".to_string(),
                "-".to_string(),
                "配置错误".to_string(),
                "-".to_string(),
            ),
        };
        rows.push(InstanceRow {
            name: instance.name.clone(),
            version,
            port,
            status,
            datadir,
        });
    }
    Ok(rows)
}

pub fn start_instance(ctx: &mut Ctx<'_>, name: &str) -> Result<String> {
    let instance = ctx.config.require(name)?.clone();
    let (settings, ini_path) = ctx.settings_for(&instance)?;
    let mut lifecycle = ctx.lifecycle();
    let pid = lifecycle.start(&instance, &settings, &ini_path)?;
    if let Some(stored) = ctx.config.find_mut(name) {
        stored.last_pid = Some(pid);
    }
    ctx.save()?;
    let port = settings.require_port(&ini_path)?;
    Ok(format!(
        "已启动 `{name}`（进程号 {pid}，端口 {port}）\n  数据目录：{}",
        settings.require_datadir(&ini_path)?.display()
    ))
}

pub fn stop_instance(ctx: &mut Ctx<'_>, name: &str) -> Result<String> {
    let instance = ctx.config.require(name)?.clone();
    let (settings, ini_path) = ctx.settings_for(&instance)?;
    let mut lifecycle = ctx.lifecycle();
    lifecycle.stop(&instance, &settings, &ini_path)?;
    if let Some(stored) = ctx.config.find_mut(name) {
        stored.last_pid = None;
    }
    ctx.save()?;
    Ok(format!("已停止 `{name}`"))
}

pub fn instance_status(ctx: &Ctx<'_>, name: &str) -> Result<String> {
    let instance = ctx.config.require(name)?;
    let (settings, ini_path) = ctx.settings_for(instance)?;
    let status = ctx.lifecycle().status(instance, &settings, &ini_path)?;
    Ok(format!(
        "{name}: {} (port {})",
        status.label(),
        settings.require_port(&ini_path)?
    ))
}

/// Stores the root password for an instance and proves it works when the
/// server is reachable, so a typo is caught immediately instead of on the next
/// unrelated command.
pub fn login(ctx: &mut Ctx<'_>, name: &str, password: &str) -> Result<String> {
    let instance = ctx.config.require(name)?.clone();
    let (settings, ini_path) = ctx.settings_for(&instance)?;

    let mut candidate = instance.clone();
    candidate.root_password = password.to_string();
    let client = ctx.client(&candidate, &settings, &ini_path);

    let mut message = format!("已更新 `{name}` 的 root 密码");
    if client.is_reachable() {
        client
            .query("SELECT 1")
            .map_err(|error| AppError::mysql(format!("服务正在运行，但拒绝了该凭据：{error}")))?;
        message.push_str("\n  verified against the running server");
    } else {
        message.push_str("\n  the server is not running, so the password was not verified yet");
    }

    if let Some(stored) = ctx.config.find_mut(name) {
        stored.root_password = password.to_string();
    }
    ctx.save()?;
    Ok(message)
}

fn ensure_running(ctx: &Ctx<'_>, instance: &Instance) -> Result<()> {
    let (settings, ini_path) = ctx.settings_for(instance)?;
    match ctx.lifecycle().status(instance, &settings, &ini_path)? {
        Status::Running => Ok(()),
        Status::Stopped => Err(AppError::mysql(format!(
            "实例 `{}` 未在运行；请先执行 `mm start {}`",
            instance.name, instance.name
        ))),
    }
}

pub fn add_user(
    ctx: &mut Ctx<'_>,
    instance_name: &str,
    user: &str,
    host: &str,
    password: &str,
) -> Result<String> {
    sql::validate_user_name(user)?;
    sql::validate_host(host)?;
    let instance = ctx.config.require(instance_name)?.clone();
    ensure_running(ctx, &instance)?;
    let (settings, ini_path) = ctx.settings_for(&instance)?;
    let client = ctx.client(&instance, &settings, &ini_path);
    let version = client.detect_version()?;

    if !version.supports_create_user_if_not_exists() {
        let existing = client.existing_accounts().unwrap_or_default();
        let target = format!("{user}@{host}");
        if existing
            .iter()
            .any(|item| item.eq_ignore_ascii_case(&target))
        {
            return Err(AppError::mysql(format!(
                "用户 `{target}` 在 `{instance_name}` 上已存在"
            )));
        }
    }

    let plan = sql::create_user_sql(version, user, host, password)?;
    client.run_sql(&plan.statements)?;
    Ok(format!(
        "已在 `{instance_name}` 上创建用户 `{user}`@`{host}`"
    ))
}

pub fn remove_user(
    ctx: &mut Ctx<'_>,
    instance_name: &str,
    user: &str,
    host: &str,
) -> Result<String> {
    sql::validate_user_name(user)?;
    sql::validate_host(host)?;
    if sql::is_protected_user(user) {
        return Err(AppError::usage(format!("拒绝删除受保护的账号 `{user}`")));
    }
    let instance = ctx.config.require(instance_name)?.clone();
    ensure_running(ctx, &instance)?;
    let (settings, ini_path) = ctx.settings_for(&instance)?;
    let client = ctx.client(&instance, &settings, &ini_path);
    let plan = sql::drop_user_sql(user, host)?;
    client.run_sql(&plan.statements)?;

    let before = ctx.config.bindings.len();
    ctx.config
        .bindings
        .retain(|item| !(item.instance == instance_name && item.user == user));
    let dropped_bindings = before - ctx.config.bindings.len();
    ctx.save()?;

    let mut message = format!("已删除 `{instance_name}` 上的用户 `{user}`@`{host}`");
    if dropped_bindings > 0 {
        message.push_str(&format!("\n同时清除了 {dropped_bindings} 条绑定记录"));
    }
    Ok(message)
}

pub fn list_users(ctx: &Ctx<'_>, instance_name: &str) -> Result<Vec<String>> {
    let instance = ctx.config.require(instance_name)?;
    ensure_running(ctx, instance)?;
    let (settings, ini_path) = ctx.settings_for(instance)?;
    let client = ctx.client(instance, &settings, &ini_path);
    let mut accounts = client.existing_accounts()?;
    accounts.sort();
    Ok(accounts)
}

pub fn add_database(ctx: &mut Ctx<'_>, instance_name: &str, database: &str) -> Result<String> {
    sql::validate_schema_name(database)?;
    let instance = ctx.config.require(instance_name)?.clone();
    ensure_running(ctx, &instance)?;
    let (settings, ini_path) = ctx.settings_for(&instance)?;
    let client = ctx.client(&instance, &settings, &ini_path);
    let version = client.detect_version()?;
    let plan = sql::create_database_sql(version, database)?;
    client.run_sql(&plan.statements)?;
    Ok(format!("已在 `{instance_name}` 上创建数据库 `{database}`"))
}

pub fn remove_database(ctx: &mut Ctx<'_>, instance_name: &str, database: &str) -> Result<String> {
    sql::validate_schema_name(database)?;
    if sql::is_protected_schema(database) {
        return Err(AppError::usage(format!("拒绝删除系统数据库 `{database}`")));
    }
    let instance = ctx.config.require(instance_name)?.clone();
    ensure_running(ctx, &instance)?;
    let (settings, ini_path) = ctx.settings_for(&instance)?;
    let client = ctx.client(&instance, &settings, &ini_path);
    let plan = sql::drop_database_sql(database)?;
    client.run_sql(&plan.statements)?;

    let before = ctx.config.bindings.len();
    ctx.config
        .bindings
        .retain(|item| !(item.instance == instance_name && item.database == database));
    let dropped_bindings = before - ctx.config.bindings.len();
    ctx.save()?;

    let mut message = format!("已删除 `{instance_name}` 上的数据库 `{database}`");
    if dropped_bindings > 0 {
        message.push_str(&format!("\n同时清除了 {dropped_bindings} 条绑定记录"));
    }
    Ok(message)
}

pub fn list_databases(ctx: &Ctx<'_>, instance_name: &str) -> Result<Vec<String>> {
    let instance = ctx.config.require(instance_name)?;
    ensure_running(ctx, instance)?;
    let (settings, ini_path) = ctx.settings_for(instance)?;
    let client = ctx.client(instance, &settings, &ini_path);
    let mut schemas = client.existing_schemas()?;
    schemas.sort();
    Ok(schemas)
}

/// Binds a user to exactly one database: grant on that schema, revoke
/// everything else, then report the privileges that actually took effect.
pub fn bind(
    ctx: &mut Ctx<'_>,
    instance_name: &str,
    database: &str,
    user: &str,
    host: &str,
) -> Result<String> {
    sql::validate_schema_name(database)?;
    sql::validate_user_name(user)?;
    sql::validate_host(host)?;
    if sql::is_protected_schema(database) {
        return Err(AppError::usage(format!(
            "拒绝把账号绑定到系统数据库 `{database}`"
        )));
    }
    if sql::is_protected_user(user) {
        return Err(AppError::usage(format!(
            "拒绝绑定受保护的账号 `{user}`；绑定会回收其全局权限"
        )));
    }

    let instance = ctx.config.require(instance_name)?.clone();
    ensure_running(ctx, &instance)?;

    if let Some(existing) = ctx.config.bindings.iter().find(|item| {
        item.instance == instance_name && item.user == user && item.database != database
    }) {
        return Err(AppError::usage(format!(
            "用户 `{user}` 已绑定到 `{}`；请先执行 `mm unbind {instance_name} {user}`",
            existing.database
        )));
    }

    let (settings, ini_path) = ctx.settings_for(&instance)?;
    let client = ctx.client(&instance, &settings, &ini_path);

    // Binding revokes every global privilege. Refuse before touching the server
    // if the account currently holds any, so an administrative account other
    // than the known names cannot be silently gutted.
    let before = client.show_grants(user, host)?;
    let global: Vec<&String> = before
        .iter()
        .filter(|line| sql::grant_line_confers_privileges(line))
        .filter(|line| sql::schema_from_grant_line(line).is_none())
        .collect();
    if !global.is_empty() {
        return Err(AppError::usage(format!(
            "拒绝绑定 `{user}`：该账号持有全局权限，绑定会将其回收：\n  {}",
            global
                .iter()
                .map(|line| line.as_str())
                .collect::<Vec<_>>()
                .join("\n  ")
        )));
    }

    let plan = sql::bind_sql(database, user, host)?;
    client.run_sql(&plan.statements)?;

    let grants = client.show_grants(user, host)?;
    let offending: Vec<&String> = grants
        .iter()
        .filter(|line| sql::grant_line_confers_privileges(line))
        .filter(|line| {
            sql::schema_from_grant_line(line)
                .map(|schema| !schema.eq_ignore_ascii_case(database))
                .unwrap_or(true)
        })
        .collect();
    if !offending.is_empty() {
        return Err(AppError::mysql(format!(
            "账号 `{user}` 在 `{database}` 之外仍持有权限：\n  {}",
            offending
                .iter()
                .map(|line| line.as_str())
                .collect::<Vec<_>>()
                .join("\n  ")
        )));
    }

    ctx.config
        .bindings
        .retain(|item| !(item.instance == instance_name && item.user == user));
    ctx.config.bindings.push(Binding {
        instance: instance_name.to_string(),
        database: database.to_string(),
        user: user.to_string(),
    });
    ctx.save()?;

    Ok(format!(
        "已将 `{user}`@`{host}` 绑定到 `{instance_name}` 上的 `{database}`\n  实际生效的权限：\n    {}",
        grants.join("\n    ")
    ))
}

pub fn unbind(ctx: &mut Ctx<'_>, instance_name: &str, user: &str, host: &str) -> Result<String> {
    sql::validate_user_name(user)?;
    sql::validate_host(host)?;
    if sql::is_protected_user(user) {
        return Err(AppError::usage(format!("拒绝解绑受保护的账号 `{user}`")));
    }
    let instance = ctx.config.require(instance_name)?.clone();

    let binding = ctx
        .config
        .bindings
        .iter()
        .find(|item| item.instance == instance_name && item.user == user)
        .cloned()
        .ok_or_else(|| {
            AppError::usage(format!("用户 `{user}` 在 `{instance_name}` 上没有绑定记录"))
        })?;

    ensure_running(ctx, &instance)?;
    let (settings, ini_path) = ctx.settings_for(&instance)?;
    let client = ctx.client(&instance, &settings, &ini_path);
    let plan = sql::unbind_sql(&binding.database, user, host)?;
    client.run_sql(&plan.statements)?;

    ctx.config
        .bindings
        .retain(|item| !(item.instance == instance_name && item.user == user));
    ctx.save()?;
    Ok(format!(
        "已解除 `{user}` 与 `{instance_name}` 上 `{}` 的绑定",
        binding.database
    ))
}

pub struct WorkflowOutcome {
    pub instance: String,
    pub database: String,
    pub username: String,
    pub password: String,
    pub host: String,
    pub port: u16,
    pub started: bool,
}

/// One-shot test workflow: ensure the server runs, then create a random
/// database and user, bind them exclusively, and prove the account works.
pub fn workflow(ctx: &mut Ctx<'_>, instance_name: &str) -> Result<WorkflowOutcome> {
    let instance = ctx.config.require(instance_name)?.clone();
    let (settings, ini_path) = ctx.settings_for(&instance)?;
    let mut lifecycle = ctx.lifecycle();

    let started = match lifecycle.status(&instance, &settings, &ini_path)? {
        Status::Running => false,
        Status::Stopped => {
            let pid = lifecycle.start(&instance, &settings, &ini_path)?;
            if let Some(stored) = ctx.config.find_mut(instance_name) {
                stored.last_pid = Some(pid);
            }
            ctx.save()?;
            true
        }
    };

    let (settings, ini_path) = ctx.settings_for(&instance)?;
    let client = ctx.client(&instance, &settings, &ini_path);
    let version = client.detect_version()?;

    // Failing to enumerate what already exists would silently disable the
    // collision check below, so surface it instead of defaulting to empty.
    let existing_schemas = client.existing_schemas().map_err(|error| {
        AppError::mysql(format!(
            "cannot list existing databases on `{instance_name}`: {error}"
        ))
    })?;
    let database = unique_name(&existing_schemas, sql::random_database_name)?;
    let existing_accounts = client.existing_accounts().map_err(|error| {
        AppError::mysql(format!(
            "cannot list existing accounts on `{instance_name}`: {error}"
        ))
    })?;
    let username = unique_name(&existing_accounts, sql::random_user_name)?;
    let password = sql::random_password();
    let host = "localhost".to_string();

    client.run_sql(&sql::create_database_sql(version, &database)?.statements)?;
    client.run_sql(&sql::create_user_sql(version, &username, &host, &password)?.statements)?;
    client.run_sql(&sql::bind_sql(&database, &username, &host)?.statements)?;
    client.verify_account(&username, &password, &database)?;

    ctx.config
        .bindings
        .retain(|item| !(item.instance == instance_name && item.user == username));
    ctx.config.bindings.push(Binding {
        instance: instance_name.to_string(),
        database: database.clone(),
        user: username.clone(),
    });
    ctx.config.workflow = Some(WorkflowRecord {
        last_run: timestamp(),
        instance: instance_name.to_string(),
        database: database.clone(),
        username: username.clone(),
        password: password.clone(),
    });
    ctx.save()?;

    Ok(WorkflowOutcome {
        instance: instance_name.to_string(),
        database,
        username,
        password,
        host,
        port: settings.require_port(&ini_path)?,
        started,
    })
}

/// Picks the first generated name that is not already present. Returns an error
/// rather than handing back a name it knows collides, because that would turn a
/// retryable naming problem into an opaque SQL failure downstream.
fn unique_name(existing: &[String], generator: fn() -> String) -> Result<String> {
    for _ in 0..64 {
        let candidate = generator();
        let clash = existing.iter().any(|item| {
            item.eq_ignore_ascii_case(&candidate) || item.starts_with(&format!("{candidate}@"))
        });
        if !clash {
            return Ok(candidate);
        }
    }
    Err(AppError::general(
        "尝试 64 次后仍无法生成不重复的名称".to_string(),
    ))
}

fn timestamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("unix:{}", now.as_secs())
}

/// Validates the host flag once, used by both CLI and interactive paths.
pub fn default_host() -> &'static str {
    "localhost"
}

pub fn instance_names(config: &Config) -> Vec<String> {
    config
        .instances
        .iter()
        .map(|item| item.name.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_name_never_returns_a_colliding_database_name() {
        let existing = vec!["db_aaaaaa".to_string()];
        for _ in 0..16 {
            let candidate = unique_name(&existing, sql::random_database_name).unwrap();
            assert!(!existing.contains(&candidate));
            assert!(sql::validate_schema_name(&candidate).is_ok());
        }
    }

    #[test]
    fn unique_name_errors_when_every_candidate_collides() {
        // A generator that always yields the taken name must fail loudly rather
        // than return a name known to collide.
        fn constant() -> String {
            "db_aaaaaa".to_string()
        }
        let existing = vec!["db_aaaaaa".to_string()];
        let error = unique_name(&existing, constant).unwrap_err();
        assert!(error.to_string().contains("不重复的名称"));
    }

    #[test]
    fn binding_root_is_refused_before_any_sql_runs() {
        let runner = crate::process::SystemRunner;
        let mut config = Config::default();
        config.instances.push(Instance {
            name: "x".to_string(),
            home: "C:/x".to_string(),
            my_ini: None,
            root_password: String::new(),
            last_pid: None,
        });
        let mut ctx = Ctx::new(config, &runner);
        let error = bind(&mut ctx, "x", "appdb", "root", "localhost").unwrap_err();
        assert!(error.to_string().contains("受保护的账号"));
    }

    #[test]
    fn unbinding_root_is_refused() {
        let runner = crate::process::SystemRunner;
        let mut ctx = Ctx::new(Config::default(), &runner);
        let error = unbind(&mut ctx, "x", "root", "localhost").unwrap_err();
        assert!(error.to_string().contains("受保护的账号"));
    }

    #[test]
    fn unique_name_avoids_account_collisions() {
        let existing = vec!["u_abc123@localhost".to_string()];
        for _ in 0..16 {
            let candidate = unique_name(&existing, sql::random_user_name).unwrap();
            assert_ne!(candidate, "u_abc123");
        }
    }

    #[test]
    fn default_host_is_localhost() {
        assert_eq!(default_host(), "localhost");
    }

    #[test]
    fn instance_names_are_collected_in_order() {
        let mut config = Config::default();
        config.instances.push(Instance {
            name: "a".to_string(),
            home: "C:/a".to_string(),
            my_ini: None,
            root_password: String::new(),
            last_pid: None,
        });
        config.instances.push(Instance {
            name: "b".to_string(),
            home: "C:/b".to_string(),
            my_ini: None,
            root_password: String::new(),
            last_pid: None,
        });
        assert_eq!(instance_names(&config), vec!["a", "b"]);
    }

    #[test]
    fn add_instance_rejects_a_directory_without_mysqld() {
        // A real directory that exists but is not a MySQL installation, so the
        // check under test is the mysqld.exe lookup rather than the is_dir one.
        let dir = std::env::temp_dir().join(format!("mm-notmysql-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        let runner = crate::process::SystemRunner;
        let mut ctx = Ctx::new(Config::default(), &runner);
        let error =
            add_instance(&mut ctx, "bogus", &dir.display().to_string(), None, None).unwrap_err();
        assert!(
            error.to_string().contains("不是有效的 MySQL 安装"),
            "got: {error}"
        );
        assert_eq!(error.code(), crate::error::EXIT_INSTANCE);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn add_instance_rejects_a_missing_directory() {
        let runner = crate::process::SystemRunner;
        let mut ctx = Ctx::new(Config::default(), &runner);
        let error =
            add_instance(&mut ctx, "bogus", "C:/definitely/not/mysql", None, None).unwrap_err();
        assert!(error.to_string().contains("不存在"));
        assert_eq!(error.code(), crate::error::EXIT_INSTANCE);
    }

    #[test]
    fn add_instance_rejects_duplicate_names() {
        let runner = crate::process::SystemRunner;
        let mut config = Config::default();
        config.instances.push(Instance {
            name: "dup".to_string(),
            home: "C:/dup".to_string(),
            my_ini: None,
            root_password: String::new(),
            last_pid: None,
        });
        let mut ctx = Ctx::new(config, &runner);
        let error = add_instance(&mut ctx, "dup", "C:/dup", None, None).unwrap_err();
        assert!(error.to_string().contains("已登记"));
    }

    #[test]
    fn protected_schemas_cannot_be_removed() {
        let runner = crate::process::SystemRunner;
        let mut config = Config::default();
        config.instances.push(Instance {
            name: "x".to_string(),
            home: "C:/x".to_string(),
            my_ini: None,
            root_password: String::new(),
            last_pid: None,
        });
        let mut ctx = Ctx::new(config, &runner);
        let error = remove_database(&mut ctx, "x", "mysql").unwrap_err();
        assert!(error.to_string().contains("系统数据库"));
    }

    #[test]
    fn protected_users_cannot_be_removed() {
        let runner = crate::process::SystemRunner;
        let mut config = Config::default();
        config.instances.push(Instance {
            name: "x".to_string(),
            home: "C:/x".to_string(),
            my_ini: None,
            root_password: String::new(),
            last_pid: None,
        });
        let mut ctx = Ctx::new(config, &runner);
        let error = remove_user(&mut ctx, "x", "root", "localhost").unwrap_err();
        assert!(error.to_string().contains("受保护的账号"));
    }

    #[test]
    fn unknown_instance_reports_the_registration_hint() {
        let runner = crate::process::SystemRunner;
        let mut ctx = Ctx::new(Config::default(), &runner);
        let error = start_instance(&mut ctx, "ghost").unwrap_err();
        assert!(error.to_string().contains("未登记"));
    }

    #[test]
    fn remove_instance_also_drops_its_bindings() {
        let runner = crate::process::SystemRunner;
        let mut config = Config::default();
        config.instances.push(Instance {
            name: "x".to_string(),
            home: "C:/x".to_string(),
            my_ini: None,
            root_password: String::new(),
            last_pid: None,
        });
        config.bindings.push(Binding {
            instance: "x".to_string(),
            database: "db".to_string(),
            user: "u".to_string(),
        });
        let mut ctx = Ctx::new(config, &runner);
        let message = remove_instance(&mut ctx, "x").unwrap();
        assert!(message.contains("绑定记录"));
        assert!(ctx.config.instances.is_empty());
        assert!(ctx.config.bindings.is_empty());
    }
}
