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

/// Finds another registered instance that is currently running. Only one
/// instance may run at a time, so every start path consults this first.
///
/// An instance whose `my.ini` cannot be read is skipped rather than treated as
/// running: a broken configuration should not block starting a healthy one.
fn find_running_other(ctx: &Ctx<'_>, exclude: &str) -> Option<String> {
    for other in &ctx.config.instances {
        if other.name == exclude {
            continue;
        }
        let Ok((settings, ini_path)) = ctx.settings_for(other) else {
            continue;
        };
        if matches!(
            ctx.lifecycle().status(other, &settings, &ini_path),
            Ok(Status::Running)
        ) {
            return Some(other.name.clone());
        }
    }
    None
}

/// Rejects a start when another instance already owns the machine's MySQL
/// ports, so two servers can never compete for the same port.
fn ensure_no_other_running(ctx: &Ctx<'_>, name: &str) -> Result<()> {
    if let Some(running) = find_running_other(ctx, name) {
        return Err(AppError::mysql(format!(
            "实例 `{running}` 正在运行，同一时间只允许运行一个实例；请先执行 `mm stop {running}` 再启动 `{name}`"
        )));
    }
    Ok(())
}

pub fn start_instance(ctx: &mut Ctx<'_>, name: &str) -> Result<String> {
    let instance = ctx.config.require(name)?.clone();
    ensure_no_other_running(ctx, name)?;
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

/// What a wipe removed, so the caller can report it and the tests can assert on
/// it without parsing prose.
#[derive(Debug)]
pub struct ResetOutcome {
    pub instance: String,
    pub dropped_schemas: Vec<String>,
    pub dropped_accounts: Vec<String>,
    pub kept_schemas: Vec<String>,
    pub kept_accounts: Vec<String>,
    pub cleared_bindings: usize,
    pub cleared_workflows: usize,
}

/// Wipes an instance back to a freshly initialised state: every user-created
/// schema and account is dropped, while the server's own schemas (`mysql`,
/// `information_schema`, `performance_schema`, `sys`) and the accounts it ships
/// with are kept. Dropping those would break the instance instead of resetting
/// it, so they are reported as kept rather than silently ignored.
///
/// Accounts go before schemas: an account holds grants on schemas, and dropping
/// the schema first would leave those grants dangling.
pub fn reset_instance(ctx: &mut Ctx<'_>, instance_name: &str) -> Result<ResetOutcome> {
    let instance = ctx.config.require(instance_name)?.clone();
    ensure_running(ctx, &instance)?;
    let (settings, ini_path) = ctx.settings_for(&instance)?;
    let client = ctx.client(&instance, &settings, &ini_path);

    let schemas = client.existing_schemas().map_err(|error| {
        AppError::mysql(format!("无法列出 `{instance_name}` 上的数据库：{error}"))
    })?;
    let accounts = client.existing_account_pairs().map_err(|error| {
        AppError::mysql(format!("无法列出 `{instance_name}` 上的账号：{error}"))
    })?;

    let kept_schemas: Vec<String> = schemas
        .iter()
        .filter(|name| sql::is_protected_schema(name))
        .cloned()
        .collect();
    let kept_accounts: Vec<String> = accounts
        .iter()
        .filter(|(user, _)| sql::is_reserved_account(user))
        .map(|(user, host)| sql::describe_account(user, host))
        .collect();

    // Drop accounts first, then the schemas they were granted on.
    let account_plan = sql::drop_user_accounts_sql(&accounts)?;
    client.run_sql(&account_plan.statements)?;
    let schema_plan = sql::drop_user_schemas_sql(&schemas)?;
    client.run_sql(&schema_plan.statements)?;

    let dropped_accounts: Vec<String> = accounts
        .iter()
        .filter(|(user, _)| !sql::is_reserved_account(user))
        .map(|(user, host)| sql::describe_account(user, host))
        .collect();
    let dropped_schemas: Vec<String> = schemas
        .iter()
        .filter(|name| !sql::is_protected_schema(name))
        .cloned()
        .collect();

    // The bindings and workflow records now point at objects that no longer
    // exist, so keeping them would make the configuration lie about the server.
    let bindings_before = ctx.config.bindings.len();
    ctx.config
        .bindings
        .retain(|item| item.instance != instance_name);
    let workflows_before = ctx.config.workflows.len();
    ctx.config
        .workflows
        .retain(|item| item.instance != instance_name);
    ctx.save()?;

    Ok(ResetOutcome {
        instance: instance_name.to_string(),
        dropped_schemas,
        dropped_accounts,
        kept_schemas,
        kept_accounts,
        cleared_bindings: bindings_before - ctx.config.bindings.len(),
        cleared_workflows: workflows_before - ctx.config.workflows.len(),
    })
}

/// Renders a wipe outcome as the multi-line report printed by both entry points.
pub fn describe_reset(outcome: &ResetOutcome) -> String {
    let mut lines = vec![format!(
        "已清空 `{}`：数据库与账号均已恢复到全新状态",
        outcome.instance
    )];
    lines.push(format!(
        "  删除数据库 {} 个：{}",
        outcome.dropped_schemas.len(),
        if outcome.dropped_schemas.is_empty() {
            "（无）".to_string()
        } else {
            outcome.dropped_schemas.join("、")
        }
    ));
    lines.push(format!(
        "  删除账号 {} 个：{}",
        outcome.dropped_accounts.len(),
        if outcome.dropped_accounts.is_empty() {
            "（无）".to_string()
        } else {
            outcome.dropped_accounts.join("、")
        }
    ));
    lines.push(format!(
        "  保留系统数据库：{}",
        if outcome.kept_schemas.is_empty() {
            "（无）".to_string()
        } else {
            outcome.kept_schemas.join("、")
        }
    ));
    lines.push(format!(
        "  保留系统账号：{}",
        if outcome.kept_accounts.is_empty() {
            "（无）".to_string()
        } else {
            outcome.kept_accounts.join("、")
        }
    ));
    if outcome.cleared_bindings > 0 || outcome.cleared_workflows > 0 {
        lines.push(format!(
            "  同时清除了 {} 条绑定记录、{} 条 workflow 记录",
            outcome.cleared_bindings, outcome.cleared_workflows
        ));
    }
    lines.join("\n")
}

/// What a "clear the contents" run removed.
#[derive(Debug)]
pub struct ClearOutcome {
    pub instance: String,
    pub database: String,
    pub dropped_views: Vec<String>,
    pub dropped_tables: Vec<String>,
    pub dropped_routines: Vec<String>,
    pub dropped_events: Vec<String>,
    /// Accounts that still hold privileges on the schema, re-read after the
    /// clear to prove the connection information was left alone.
    pub surviving_accounts: Vec<String>,
}

impl ClearOutcome {
    pub fn dropped_count(&self) -> usize {
        self.dropped_views.len()
            + self.dropped_tables.len()
            + self.dropped_routines.len()
            + self.dropped_events.len()
    }
}

/// Empties one schema without touching the schema itself, its accounts or their
/// privileges.
///
/// This is deliberately *not* `db remove` followed by `db add`: dropping and
/// recreating the schema would revoke nothing on its own but would silently
/// change the default charset/collation, and any tooling that keyed off the
/// schema's identity would see a new database. Keeping the schema in place is
/// what makes the database name, the account name and the password keep
/// working — exactly the credentials a workflow printed.
pub fn clear_database(
    ctx: &mut Ctx<'_>,
    instance_name: &str,
    database: &str,
) -> Result<ClearOutcome> {
    sql::validate_schema_name(database)?;
    if sql::is_protected_schema(database) {
        return Err(AppError::usage(format!("拒绝清空系统数据库 `{database}`")));
    }
    let instance = ctx.config.require(instance_name)?.clone();
    ensure_running(ctx, &instance)?;
    let (settings, ini_path) = ctx.settings_for(&instance)?;
    let client = ctx.client(&instance, &settings, &ini_path);

    let existing = client.existing_schemas()?;
    if !existing.iter().any(|item| item == database) {
        return Err(AppError::usage(format!(
            "`{instance_name}` 上不存在数据库 `{database}`；可用 `mm db list {instance_name}` 查看"
        )));
    }

    let objects = client.schema_objects(database)?;
    let plan = sql::clear_schema_sql(database, &objects)?;
    client.run_sql(&plan.statements)?;

    // Verify rather than assume: report a clear only when a fresh listing really
    // is empty. A partially failed script must not be described as success.
    let after = client.schema_objects(database)?;
    if !after.is_empty() {
        return Err(AppError::mysql(format!(
            "清空后 `{database}` 中仍有残留对象：{}",
            describe_objects(&after)
        )));
    }

    // The schema must still be there, and so must the privileges of any account
    // the tool knows is bound to it.
    let still_present = client
        .existing_schemas()?
        .iter()
        .any(|item| item == database);
    if !still_present {
        return Err(AppError::mysql(format!(
            "清空后数据库 `{database}` 不存在了；这本不该发生"
        )));
    }

    let mut surviving_accounts = Vec::new();
    let bound: Vec<String> = ctx
        .config
        .bindings
        .iter()
        .filter(|item| item.instance == instance_name && item.database == database)
        .map(|item| item.user.clone())
        .collect();
    for user in bound {
        let host = default_host().to_string();
        if let Ok(grants) = client.show_grants(&user, &host) {
            let keeps_schema = grants
                .iter()
                .any(|line| sql::schema_from_grant_line(line).as_deref() == Some(database));
            if keeps_schema {
                surviving_accounts.push(format!("{user}@{host}"));
            }
        }
    }

    Ok(ClearOutcome {
        instance: instance_name.to_string(),
        database: database.to_string(),
        dropped_views: objects.views,
        dropped_tables: objects.tables,
        dropped_routines: objects
            .routines
            .iter()
            .map(|item| format!("{}({})", item.name, item.kind.keyword()))
            .collect(),
        dropped_events: objects.events,
        surviving_accounts,
    })
}

fn describe_objects(objects: &sql::SchemaObjects) -> String {
    let mut parts = Vec::new();
    if !objects.views.is_empty() {
        parts.push(format!("视图 {}", objects.views.join("、")));
    }
    if !objects.tables.is_empty() {
        parts.push(format!("表 {}", objects.tables.join("、")));
    }
    if !objects.routines.is_empty() {
        parts.push(format!(
            "存储对象 {}",
            objects
                .routines
                .iter()
                .map(|item| item.name.clone())
                .collect::<Vec<_>>()
                .join("、")
        ));
    }
    if !objects.events.is_empty() {
        parts.push(format!("事件 {}", objects.events.join("、")));
    }
    parts.join("；")
}

/// Renders a clear result, naming what was removed and confirming that the
/// connection information is untouched.
pub fn describe_clear(outcome: &ClearOutcome) -> String {
    let mut lines = vec![format!(
        "已清空 `{}` 上 `{}` 的全部内容（数据库本身与账号权限未改动）",
        outcome.instance, outcome.database
    )];
    if outcome.dropped_count() == 0 {
        lines.push("  该数据库原本就是空的，未执行任何删除。".to_string());
    } else {
        for (label, items) in [
            ("删除表", &outcome.dropped_tables),
            ("删除视图", &outcome.dropped_views),
            ("删除存储对象", &outcome.dropped_routines),
            ("删除事件", &outcome.dropped_events),
        ] {
            if !items.is_empty() {
                lines.push(format!(
                    "  {label} {} 个：{}",
                    items.len(),
                    items.join("、")
                ));
            }
        }
    }
    lines.push(format!("  数据库名保留：{}", outcome.database));
    if outcome.surviving_accounts.is_empty() {
        lines.push("  账号与密码：本工具未改动任何账号或授权".to_string());
    } else {
        lines.push(format!(
            "  账号与密码：以下账号仍保有 `{}` 的授权，连接信息不变：{}",
            outcome.database,
            outcome.surviving_accounts.join("、")
        ));
    }
    lines.join("\n")
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
    ensure_no_other_running(ctx, instance_name)?;
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
    ctx.config.workflows.push(WorkflowRecord {
        last_run: timestamp(),
        instance: instance_name.to_string(),
        database: database.clone(),
        username: username.clone(),
        password: password.clone(),
        host: host.clone(),
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

/// One row of `mm workflow list`.
pub struct WorkflowRow {
    pub index: usize,
    pub last_run: String,
    pub instance: String,
    pub database: String,
    pub username: String,
    pub host: String,
    pub password: String,
}

pub fn list_workflows(ctx: &Ctx<'_>) -> Vec<WorkflowRow> {
    ctx.config
        .workflow_history()
        .iter()
        .enumerate()
        .map(|(position, record)| WorkflowRow {
            index: position + 1,
            last_run: record.last_run.clone(),
            instance: record.instance.clone(),
            database: record.database.clone(),
            username: record.username.clone(),
            host: if record.host.is_empty() {
                default_host().to_string()
            } else {
                record.host.clone()
            },
            password: record.password.clone(),
        })
        .collect()
}

/// Full details for one workflow record, addressed by its 1-based index.
pub fn show_workflow(ctx: &Ctx<'_>, one_based: usize) -> Result<WorkflowRow> {
    let position = ctx.config.workflow_index(one_based)?;
    let record = &ctx.config.workflows[position];
    Ok(WorkflowRow {
        index: one_based,
        last_run: record.last_run.clone(),
        instance: record.instance.clone(),
        database: record.database.clone(),
        username: record.username.clone(),
        host: if record.host.is_empty() {
            default_host().to_string()
        } else {
            record.host.clone()
        },
        password: record.password.clone(),
    })
}

/// Drops a workflow's database and user, then forgets the record.
///
/// Missing objects are tolerated: if a database or account was already removed
/// by hand, the stale record can still be cleared. `DROP USER IF EXISTS` only
/// exists from 5.7.8, so older servers get an explicit existence check instead.
pub fn remove_workflow(ctx: &mut Ctx<'_>, one_based: usize) -> Result<String> {
    let position = ctx.config.workflow_index(one_based)?;
    let record = ctx.config.workflows[position].clone();
    let host = if record.host.is_empty() {
        default_host().to_string()
    } else {
        record.host.clone()
    };

    let instance = ctx.config.require(&record.instance)?.clone();
    ensure_running(ctx, &instance)?;
    let (settings, ini_path) = ctx.settings_for(&instance)?;
    let client = ctx.client(&instance, &settings, &ini_path);

    sql::validate_schema_name(&record.database)?;
    sql::validate_user_name(&record.username)?;

    let version = client.detect_version()?;

    // Drop the account first: it holds grants on the schema, and removing the
    // schema first would leave those grants dangling.
    let account = sql::account(&record.username, &host);
    if version.supports_drop_user_if_exists() {
        client.run_sql(&[format!("DROP USER IF EXISTS {account}")])?;
    } else {
        let existing = client.existing_accounts().unwrap_or_default();
        let target = format!("{}@{}", record.username, host);
        if existing
            .iter()
            .any(|item| item.eq_ignore_ascii_case(&target))
        {
            client.run_sql(&[format!("DROP USER {account}")])?;
        }
    }

    if sql::is_protected_schema(&record.database) {
        return Err(AppError::usage(format!(
            "拒绝删除系统数据库 `{}`",
            record.database
        )));
    }
    client.run_sql(&[format!(
        "DROP DATABASE IF EXISTS {}",
        sql::quote_ident(&record.database)
    )])?;

    ctx.config.workflows.remove(position);
    ctx.config
        .bindings
        .retain(|item| !(item.instance == record.instance && item.user == record.username));
    ctx.save()?;

    Ok(format!(
        "已删除 workflow 记录 {one_based}：数据库 `{}` 与用户 `{}`@`{host}` 均已移除",
        record.database, record.username
    ))
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
    fn each_workflow_run_appends_to_the_history() {
        // Mirrors what `workflow` does after a successful run: push a record.
        // A regression that overwrites instead of appending would drop the
        // earlier password, which is exactly what the user must still see.
        let mut config = Config::default();
        for (database, password) in [("db_one", "pw-one"), ("db_two", "pw-two")] {
            config.workflows.push(WorkflowRecord {
                last_run: "unix:1".to_string(),
                instance: "mysql56".to_string(),
                database: database.to_string(),
                username: format!("u_{database}"),
                password: password.to_string(),
                host: "localhost".to_string(),
            });
        }
        assert_eq!(config.workflows.len(), 2);
        assert_eq!(config.workflows[0].password, "pw-one");
        assert_eq!(config.workflows[1].password, "pw-two");
    }

    #[test]
    fn workflow_list_reports_oldest_first_with_one_based_indexes() {
        let runner = crate::process::SystemRunner;
        let mut config = Config::default();
        for (database, password) in [("db_first", "pw-1"), ("db_second", "pw-2")] {
            config.workflows.push(WorkflowRecord {
                last_run: "unix:1".to_string(),
                instance: "mysql56".to_string(),
                database: database.to_string(),
                username: format!("u_{database}"),
                password: password.to_string(),
                host: "localhost".to_string(),
            });
        }
        let ctx = Ctx::new(config, &runner);

        let rows = list_workflows(&ctx);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].index, 1);
        assert_eq!(rows[0].password, "pw-1");
        assert_eq!(rows[1].index, 2);
        assert_eq!(rows[1].password, "pw-2");

        // Showing by index resolves to the same record.
        assert_eq!(show_workflow(&ctx, 2).unwrap().password, "pw-2");
        assert!(show_workflow(&ctx, 3).is_err());
        assert!(show_workflow(&ctx, 0).is_err());
    }

    #[test]
    fn a_record_without_a_host_falls_back_to_localhost() {
        let runner = crate::process::SystemRunner;
        let mut config = Config::default();
        config.workflows.push(WorkflowRecord {
            database: "db_x".to_string(),
            username: "u_x".to_string(),
            password: "pw".to_string(),
            ..WorkflowRecord::default()
        });
        let ctx = Ctx::new(config, &runner);
        assert_eq!(list_workflows(&ctx)[0].host, "localhost");
    }

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

    /// Builds a config with two instances whose option files really exist, so
    /// the exclusivity check can reach the liveness probe.
    fn two_instance_ctx<'a>(
        runner: &'a dyn crate::process::Runner,
        dir: &std::path::Path,
    ) -> Ctx<'a> {
        let mut config = Config::default();
        for (name, port) in [("alpha", 3306u16), ("beta", 3307u16)] {
            let ini = dir.join(format!("{name}.ini"));
            std::fs::write(
                &ini,
                format!("[mysqld]\ndatadir={}\nport={port}\n", dir.display()),
            )
            .unwrap();
            config.instances.push(Instance {
                name: name.to_string(),
                home: dir.display().to_string(),
                my_ini: Some(ini.display().to_string()),
                root_password: String::new(),
                last_pid: None,
            });
        }
        Ctx::new(config, runner)
    }

    #[test]
    fn starting_a_second_instance_is_refused_while_another_runs() {
        use crate::process::testing::FakeRunner;
        let dir = std::env::temp_dir().join(format!("mm-excl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let runner = FakeRunner::new();
        // First status probe (alpha, excluded) is skipped; beta answers.
        runner.push_ok("mysqld is alive");
        let mut ctx = two_instance_ctx(&runner, &dir);

        let error = ensure_no_other_running(&ctx, "alpha").unwrap_err();
        assert!(error.to_string().contains("beta"), "got: {error}");
        assert!(
            error.to_string().contains("只允许运行一个实例"),
            "got: {error}"
        );
        assert!(error.to_string().contains("mm stop beta"), "got: {error}");
        std::fs::remove_dir_all(&dir).ok();
        // Silence the unused-mut warning on the binding.
        let _ = &mut ctx;
    }

    #[test]
    fn starting_is_allowed_when_no_other_instance_answers() {
        use crate::process::testing::FakeRunner;
        let dir = std::env::temp_dir().join(format!("mm-excl2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let runner = FakeRunner::new();
        runner.push_fail(1, "unreachable");
        let ctx = two_instance_ctx(&runner, &dir);

        ensure_no_other_running(&ctx, "alpha").expect("no other instance is running");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_instance_being_started_is_not_counted_as_a_conflict() {
        use crate::process::testing::FakeRunner;
        let dir = std::env::temp_dir().join(format!("mm-excl3-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let runner = FakeRunner::new();
        // Only one probe is issued because the excluded instance is skipped.
        runner.push_fail(1, "unreachable");
        let ctx = two_instance_ctx(&runner, &dir);

        ensure_no_other_running(&ctx, "alpha").expect("alpha itself must be ignored");
        // Exactly one probe means beta was checked and alpha was skipped.
        assert_eq!(runner.invocations().len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_broken_other_instance_does_not_block_starting() {
        use crate::process::testing::FakeRunner;
        let dir = std::env::temp_dir().join(format!("mm-excl4-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let runner = FakeRunner::new();
        let mut config = Config::default();
        // A healthy target instance.
        let ini = dir.join("ok.ini");
        std::fs::write(
            &ini,
            format!("[mysqld]\ndatadir={}\nport=3306\n", dir.display()),
        )
        .unwrap();
        config.instances.push(Instance {
            name: "ok".to_string(),
            home: dir.display().to_string(),
            my_ini: Some(ini.display().to_string()),
            root_password: String::new(),
            last_pid: None,
        });
        // Another instance whose option file is missing entirely.
        config.instances.push(Instance {
            name: "broken".to_string(),
            home: dir.display().to_string(),
            my_ini: Some(dir.join("missing.ini").display().to_string()),
            root_password: String::new(),
            last_pid: None,
        });
        let ctx = Ctx::new(config, &runner);

        ensure_no_other_running(&ctx, "ok")
            .expect("a broken sibling configuration must not block a start");
        std::fs::remove_dir_all(&dir).ok();
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

    /// Builds a context with one running instance whose `my.ini` exists, so a
    /// wipe can reach the SQL layer with the fake runner answering the listing
    /// queries.
    fn running_ctx<'a>(runner: &'a dyn crate::process::Runner, dir: &std::path::Path) -> Ctx<'a> {
        let ini = dir.join("reset.ini");
        std::fs::write(
            &ini,
            format!("[mysqld]\ndatadir={}\nport=3306\n", dir.display()),
        )
        .unwrap();
        let mut config = Config::default();
        config.instances.push(Instance {
            name: "wipe".to_string(),
            home: dir.display().to_string(),
            my_ini: Some(ini.display().to_string()),
            root_password: "pw".to_string(),
            last_pid: None,
        });
        Ctx::new(config, runner)
    }

    #[test]
    fn a_wipe_drops_user_objects_and_forgets_stale_records() {
        use crate::process::testing::FakeRunner;
        let dir = std::env::temp_dir().join(format!("mm-reset-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let runner = FakeRunner::new();
        runner.push_ok("mysqld is alive"); // ensure_running
        runner.push_ok("mysql\nsys\nappdb\nother_db"); // SHOW DATABASES
        runner.push_ok("root\tlocalhost\napp\tlocalhost"); // SELECT User, Host
        runner.push_ok(""); // DROP USER script
        runner.push_ok(""); // DROP DATABASE script
        let mut ctx = running_ctx(&runner, &dir);
        ctx.config.bindings.push(Binding {
            instance: "wipe".to_string(),
            database: "appdb".to_string(),
            user: "app".to_string(),
        });
        ctx.config.workflows.push(WorkflowRecord {
            instance: "wipe".to_string(),
            database: "appdb".to_string(),
            username: "app".to_string(),
            ..WorkflowRecord::default()
        });

        let outcome = reset_instance(&mut ctx, "wipe").unwrap();
        assert_eq!(outcome.dropped_schemas, vec!["appdb", "other_db"]);
        assert_eq!(outcome.dropped_accounts, vec!["app@localhost"]);
        assert_eq!(outcome.kept_schemas, vec!["mysql", "sys"]);
        assert_eq!(outcome.kept_accounts, vec!["root@localhost"]);
        assert_eq!(outcome.cleared_bindings, 1);
        assert_eq!(outcome.cleared_workflows, 1);
        assert!(ctx.config.bindings.is_empty());
        assert!(ctx.config.workflows.is_empty());

        // Accounts must be dropped before the schemas they hold grants on.
        let scripts = runner.stdin_writes();
        let account_script = scripts
            .iter()
            .position(|text| text.contains("DROP USER"))
            .expect("a wipe must drop accounts");
        let schema_script = scripts
            .iter()
            .position(|text| text.contains("DROP DATABASE"))
            .expect("a wipe must drop schemas");
        assert!(
            account_script < schema_script,
            "accounts must be dropped first: {scripts:?}"
        );
        assert!(scripts[account_script].contains("'app'@'localhost'"));
        assert!(!scripts[account_script].contains("'root'@'localhost'"));
        assert!(scripts[schema_script].contains("`appdb`"));
        assert!(!scripts[schema_script].contains("`mysql`"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_wipe_on_a_pristine_instance_issues_no_destructive_sql() {
        use crate::process::testing::FakeRunner;
        let dir = std::env::temp_dir().join(format!("mm-reset2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let runner = FakeRunner::new();
        runner.push_ok("mysqld is alive"); // ensure_running
        runner.push_ok("mysql\ninformation_schema\nperformance_schema\nsys");
        runner.push_ok("root\tlocalhost");
        let mut ctx = running_ctx(&runner, &dir);

        let outcome = reset_instance(&mut ctx, "wipe").unwrap();
        assert!(outcome.dropped_schemas.is_empty());
        assert!(outcome.dropped_accounts.is_empty());
        assert_eq!(outcome.kept_schemas.len(), 4);
        // `run_sql` short-circuits an empty plan, so the only scripts sent are
        // the two listing queries: no DROP may reach the server.
        for script in runner.stdin_writes() {
            assert!(
                !script.contains("DROP"),
                "unexpected destructive SQL: {script}"
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_wipe_requires_the_instance_to_be_running() {
        use crate::process::testing::FakeRunner;
        let dir = std::env::temp_dir().join(format!("mm-reset3-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let runner = FakeRunner::new();
        runner.push_fail(1, "unreachable"); // ensure_running
        let mut ctx = running_ctx(&runner, &dir);
        let error = reset_instance(&mut ctx, "wipe").unwrap_err();
        assert!(error.to_string().contains("未在运行"), "got: {error}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_wipe_refuses_an_unregistered_instance() {
        let runner = crate::process::SystemRunner;
        let mut ctx = Ctx::new(Config::default(), &runner);
        let error = reset_instance(&mut ctx, "ghost").unwrap_err();
        assert!(error.to_string().contains("未登记"));
    }

    #[test]
    fn the_wipe_report_names_what_was_dropped_and_kept() {
        let outcome = ResetOutcome {
            instance: "wipe".to_string(),
            dropped_schemas: vec!["appdb".to_string()],
            dropped_accounts: vec!["app@localhost".to_string()],
            kept_schemas: vec!["mysql".to_string()],
            kept_accounts: vec!["root@localhost".to_string()],
            cleared_bindings: 1,
            cleared_workflows: 2,
        };
        let text = describe_reset(&outcome);
        assert!(text.contains("wipe"));
        assert!(text.contains("appdb"));
        assert!(text.contains("app@localhost"));
        assert!(text.contains("mysql"));
        assert!(text.contains("root@localhost"));
        assert!(text.contains("1 条绑定记录"));
        assert!(text.contains("2 条 workflow 记录"));
    }

    #[test]
    fn the_wipe_report_says_none_when_nothing_was_dropped() {
        let outcome = ResetOutcome {
            instance: "wipe".to_string(),
            dropped_schemas: Vec::new(),
            dropped_accounts: Vec::new(),
            kept_schemas: vec!["mysql".to_string()],
            kept_accounts: vec!["root@localhost".to_string()],
            cleared_bindings: 0,
            cleared_workflows: 0,
        };
        let text = describe_reset(&outcome);
        assert!(text.contains("删除数据库 0 个：（无）"));
        // No stale-record line when nothing was cleared.
        assert!(!text.contains("同时清除了"));
    }

    /// Queues the four catalog queries a clear issues, in the order the client
    /// runs them.
    fn push_schema_listing(
        runner: &crate::process::testing::FakeRunner,
        views: &str,
        tables: &str,
        routines: &str,
        events: &str,
    ) {
        runner.push_ok(views);
        runner.push_ok(tables);
        runner.push_ok(routines);
        runner.push_ok(events);
    }

    #[test]
    fn clearing_a_database_drops_its_objects_but_not_the_database() {
        use crate::process::testing::FakeRunner;
        let dir = std::env::temp_dir().join(format!("mm-clear-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let runner = FakeRunner::new();
        runner.push_ok("mysqld is alive"); // ensure_running
        runner.push_ok("mysql\nappdb"); // SHOW DATABASES
        push_schema_listing(&runner, "v_summary", "orders\nusers", "", "nightly");
        runner.push_ok(""); // the clear script
        push_schema_listing(&runner, "", "", "", ""); // verification
        runner.push_ok("mysql\nappdb"); // schema still present
        runner.push_ok(
            "GRANT USAGE ON *.* TO 'app'@'localhost'\n\
             GRANT ALL PRIVILEGES ON `appdb`.* TO 'app'@'localhost'", // SHOW GRANTS
        );
        let mut ctx = running_ctx(&runner, &dir);
        ctx.config.bindings.push(Binding {
            instance: "wipe".to_string(),
            database: "appdb".to_string(),
            user: "app".to_string(),
        });

        let outcome = clear_database(&mut ctx, "wipe", "appdb").unwrap();
        assert_eq!(outcome.database, "appdb");
        assert_eq!(outcome.dropped_tables, vec!["orders", "users"]);
        assert_eq!(outcome.dropped_views, vec!["v_summary"]);
        assert_eq!(outcome.dropped_events, vec!["nightly"]);
        assert_eq!(outcome.dropped_count(), 4);
        // The account still holds its grant on the schema: that is the whole
        // point of clearing instead of dropping.
        assert_eq!(outcome.surviving_accounts, vec!["app@localhost"]);

        let scripts = runner.stdin_writes();
        let clear_script = scripts
            .iter()
            .find(|sql| sql.contains("DROP TABLE"))
            .expect("a clear must drop tables");
        assert!(!clear_script.contains("DROP DATABASE"));
        assert!(!clear_script.contains("DROP USER"));
        // The binding must survive: the schema and the account both still exist.
        assert_eq!(ctx.config.bindings.len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn clearing_an_empty_database_sends_no_destructive_sql() {
        use crate::process::testing::FakeRunner;
        let dir = std::env::temp_dir().join(format!("mm-clear2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let runner = FakeRunner::new();
        runner.push_ok("mysqld is alive"); // ensure_running
        runner.push_ok("mysql\nappdb"); // SHOW DATABASES
        push_schema_listing(&runner, "", "", "", ""); // nothing to drop
        push_schema_listing(&runner, "", "", "", ""); // verification
        runner.push_ok("mysql\nappdb"); // schema still present
        let mut ctx = running_ctx(&runner, &dir);

        let outcome = clear_database(&mut ctx, "wipe", "appdb").unwrap();
        assert_eq!(outcome.dropped_count(), 0);
        for script in runner.stdin_writes() {
            assert!(
                !script.contains("DROP"),
                "unexpected destructive SQL: {script}"
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn clearing_a_missing_database_is_refused_before_any_ddl() {
        use crate::process::testing::FakeRunner;
        let dir = std::env::temp_dir().join(format!("mm-clear3-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let runner = FakeRunner::new();
        runner.push_ok("mysqld is alive"); // ensure_running
        runner.push_ok("mysql\nother"); // SHOW DATABASES: appdb absent
        let mut ctx = running_ctx(&runner, &dir);

        let error = clear_database(&mut ctx, "wipe", "appdb").unwrap_err();
        assert!(error.to_string().contains("不存在数据库"), "got: {error}");
        for script in runner.stdin_writes() {
            assert!(!script.contains("DROP"), "no DDL may run: {script}");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn clearing_a_system_database_is_refused() {
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
        let error = clear_database(&mut ctx, "x", "mysql").unwrap_err();
        assert!(error.to_string().contains("系统数据库"));
        assert_eq!(error.code(), crate::error::EXIT_USAGE);
    }

    #[test]
    fn a_clear_that_leaves_objects_behind_is_reported_as_a_failure() {
        use crate::process::testing::FakeRunner;
        let dir = std::env::temp_dir().join(format!("mm-clear4-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let runner = FakeRunner::new();
        runner.push_ok("mysqld is alive"); // ensure_running
        runner.push_ok("mysql\nappdb"); // SHOW DATABASES
        push_schema_listing(&runner, "", "orders", "", ""); // before
        runner.push_ok(""); // clear script
        push_schema_listing(&runner, "", "orders", "", ""); // still there
        let mut ctx = running_ctx(&runner, &dir);

        let error = clear_database(&mut ctx, "wipe", "appdb").unwrap_err();
        assert!(error.to_string().contains("仍有残留对象"), "got: {error}");
        assert!(error.to_string().contains("orders"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_clear_report_confirms_the_connection_information_is_unchanged() {
        let outcome = ClearOutcome {
            instance: "wipe".to_string(),
            database: "appdb".to_string(),
            dropped_views: vec!["v".to_string()],
            dropped_tables: vec!["t1".to_string(), "t2".to_string()],
            dropped_routines: vec!["p(PROCEDURE)".to_string()],
            dropped_events: Vec::new(),
            surviving_accounts: vec!["app@localhost".to_string()],
        };
        let text = describe_clear(&outcome);
        assert!(text.contains("appdb"));
        assert!(text.contains("删除表 2 个"));
        assert!(text.contains("删除视图 1 个"));
        assert!(text.contains("删除存储对象 1 个"));
        assert!(text.contains("数据库名保留：appdb"));
        assert!(text.contains("连接信息不变"));
        assert!(text.contains("app@localhost"));
    }

    #[test]
    fn the_clear_report_says_so_when_the_database_was_already_empty() {
        let outcome = ClearOutcome {
            instance: "wipe".to_string(),
            database: "appdb".to_string(),
            dropped_views: Vec::new(),
            dropped_tables: Vec::new(),
            dropped_routines: Vec::new(),
            dropped_events: Vec::new(),
            surviving_accounts: Vec::new(),
        };
        let text = describe_clear(&outcome);
        assert!(text.contains("原本就是空的"));
        assert!(text.contains("未改动任何账号或授权"));
    }
}
