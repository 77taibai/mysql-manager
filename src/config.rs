use crate::error::{AppError, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const CONFIG_FILE_NAME: &str = "config.toml";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Instance {
    pub name: String,
    pub home: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub my_ini: Option<String>,
    #[serde(default)]
    pub root_password: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_pid: Option<u32>,
}

impl Instance {
    pub fn home_path(&self) -> PathBuf {
        PathBuf::from(&self.home)
    }

    pub fn my_ini_path(&self) -> PathBuf {
        match &self.my_ini {
            Some(path) => PathBuf::from(path),
            None => self.home_path().join("my.ini"),
        }
    }

    pub fn mysqld_exe(&self) -> PathBuf {
        self.home_path().join("bin").join("mysqld.exe")
    }

    pub fn mysql_exe(&self) -> PathBuf {
        self.home_path().join("bin").join("mysql.exe")
    }

    pub fn mysqladmin_exe(&self) -> PathBuf {
        self.home_path().join("bin").join("mysqladmin.exe")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Binding {
    pub instance: String,
    pub database: String,
    pub user: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkflowRecord {
    #[serde(default)]
    pub last_run: String,
    #[serde(default)]
    pub instance: String,
    #[serde(default)]
    pub database: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
    #[serde(default = "default_host_value")]
    pub host: String,
}

fn default_host_value() -> String {
    "localhost".to_string()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub instances: Vec<Instance>,
    #[serde(default)]
    pub bindings: Vec<Binding>,
    /// Every workflow run is appended here so an earlier run's generated
    /// password stays retrievable. `Vec` rather than a single record on
    /// purpose: overwriting would silently destroy credentials the user still
    /// needs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workflows: Vec<WorkflowRecord>,
    /// Legacy single-record field from earlier versions. Read only, so an
    /// existing `config.toml` keeps its last workflow after upgrading.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<WorkflowRecord>,
}

impl Config {
    pub fn find(&self, name: &str) -> Option<&Instance> {
        self.instances.iter().find(|item| item.name == name)
    }

    pub fn find_mut(&mut self, name: &str) -> Option<&mut Instance> {
        self.instances.iter_mut().find(|item| item.name == name)
    }

    pub fn require(&self, name: &str) -> Result<&Instance> {
        self.find(name).ok_or_else(|| {
            AppError::instance(format!(
                "实例 `{name}` 未登记；请运行 `mm instance add {name} --home <路径>`"
            ))
        })
    }

    /// Folds a legacy single `workflow` record into the list so it stays
    /// visible after upgrading, then clears the legacy field.
    pub fn migrate_legacy_workflow(&mut self) {
        if let Some(legacy) = self.workflow.take() {
            self.workflows.push(legacy);
        }
    }

    /// Returns the workflow history, oldest first.
    pub fn workflow_history(&self) -> &[WorkflowRecord] {
        &self.workflows
    }

    /// Resolves a 1-based index typed by the user into a record position.
    pub fn workflow_index(&self, one_based: usize) -> Result<usize> {
        if one_based == 0 || one_based > self.workflows.len() {
            return Err(AppError::usage(format!(
                "序号 {one_based} 超出范围；当前共 {} 条 workflow 记录",
                self.workflows.len()
            )));
        }
        Ok(one_based - 1)
    }
}

/// Resolves `config.toml` next to the running executable, not the current
/// working directory, so `cargo run` and a direct launch agree.
pub fn config_path() -> Result<PathBuf> {
    let exe = std::env::current_exe()
        .map_err(|error| AppError::general(format!("无法定位可执行文件：{error}")))?;
    let dir = exe
        .parent()
        .ok_or_else(|| AppError::general("可执行文件路径没有上级目录".to_string()))?;
    Ok(dir.join(CONFIG_FILE_NAME))
}

pub fn load_from(path: &Path) -> Result<Config> {
    if !path.exists() {
        return Ok(Config::default());
    }
    let text = std::fs::read_to_string(path)
        .map_err(|error| AppError::general(format!("无法读取 {}：{error}", path.display())))?;
    if text.trim().is_empty() {
        return Ok(Config::default());
    }
    let mut config: Config = toml::from_str(&text)
        .map_err(|error| AppError::general(format!("无法解析 {}：{error}", path.display())))?;
    config.migrate_legacy_workflow();
    Ok(config)
}

pub fn save_to(path: &Path, config: &Config) -> Result<()> {
    let text = toml::to_string_pretty(config)
        .map_err(|error| AppError::general(format!("无法序列化配置：{error}")))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            AppError::general(format!("无法创建 {}：{error}", parent.display()))
        })?;
    }
    // Write to a sibling temporary file first so a crash cannot leave a
    // half-written configuration behind.
    let temp = path.with_extension("toml.tmp");
    std::fs::write(&temp, text)
        .map_err(|error| AppError::general(format!("无法写入 {}：{error}", temp.display())))?;
    std::fs::rename(&temp, path).map_err(|error| {
        let _ = std::fs::remove_file(&temp);
        AppError::general(format!("无法替换 {}：{error}", path.display()))
    })
}

pub fn load() -> Result<Config> {
    load_from(&config_path()?)
}

pub fn save(config: &Config) -> Result<()> {
    save_to(&config_path()?, config)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Config {
        Config {
            instances: vec![Instance {
                name: "mysql56".to_string(),
                home: r"C:\mysql\5.6".to_string(),
                my_ini: None,
                root_password: "s3cret".to_string(),
                last_pid: Some(4321),
            }],
            bindings: vec![Binding {
                instance: "mysql56".to_string(),
                database: "app".to_string(),
                user: "app_user".to_string(),
            }],
            workflows: vec![WorkflowRecord {
                last_run: "2026-02-14T10:00:00+08:00".to_string(),
                instance: "mysql56".to_string(),
                database: "db_abc123".to_string(),
                username: "u_abc123".to_string(),
                password: "pw".to_string(),
                host: "localhost".to_string(),
            }],
            workflow: None,
        }
    }

    #[test]
    fn round_trips_through_a_file() {
        let dir = std::env::temp_dir().join(format!("mm-config-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");

        let original = sample();
        save_to(&path, &original).unwrap();
        let loaded = load_from(&path).unwrap();

        assert_eq!(loaded.instances.len(), 1);
        assert_eq!(loaded.instances[0].name, "mysql56");
        assert_eq!(loaded.instances[0].root_password, "s3cret");
        assert_eq!(loaded.instances[0].last_pid, Some(4321));
        assert_eq!(loaded.bindings.len(), 1);
        assert_eq!(loaded.workflows.len(), 1);
        assert_eq!(loaded.workflows[0].database, "db_abc123");
        assert_eq!(loaded.workflows[0].password, "pw");

        // No temporary file may survive a successful save.
        assert!(!path.with_extension("toml.tmp").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_file_yields_an_empty_configuration() {
        let path = std::env::temp_dir().join("mm-config-does-not-exist.toml");
        std::fs::remove_file(&path).ok();
        let config = load_from(&path).unwrap();
        assert!(config.instances.is_empty());
        assert!(config.bindings.is_empty());
        assert!(config.workflows.is_empty());
        assert!(config.workflow.is_none());
    }

    #[test]
    fn workflow_history_keeps_every_run() {
        let dir = std::env::temp_dir().join(format!("mm-wfhist-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");

        let mut config = Config::default();
        for (database, password) in [("db_first", "pw-one"), ("db_second", "pw-two")] {
            config.workflows.push(WorkflowRecord {
                last_run: "unix:1".to_string(),
                instance: "mysql56".to_string(),
                database: database.to_string(),
                username: format!("u_{database}"),
                password: password.to_string(),
                host: "localhost".to_string(),
            });
        }
        save_to(&path, &config).unwrap();

        let loaded = load_from(&path).unwrap();
        // The earlier run's password must still be present.
        assert_eq!(loaded.workflows.len(), 2);
        assert_eq!(loaded.workflows[0].database, "db_first");
        assert_eq!(loaded.workflows[0].password, "pw-one");
        assert_eq!(loaded.workflows[1].password, "pw-two");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_legacy_single_workflow_is_migrated_into_the_list() {
        let dir = std::env::temp_dir().join(format!("mm-wflegacy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            "[workflow]\n\
             last_run = 'unix:1'\n\
             instance = 'mysql56'\n\
             database = 'db_old'\n\
             username = 'u_old'\n\
             password = 'old-pw'\n",
        )
        .unwrap();

        let loaded = load_from(&path).unwrap();
        assert_eq!(loaded.workflows.len(), 1);
        assert_eq!(loaded.workflows[0].database, "db_old");
        assert_eq!(loaded.workflows[0].password, "old-pw");
        assert!(
            loaded.workflow.is_none(),
            "the legacy field must be cleared after migration"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn workflow_index_is_one_based_and_bounds_checked() {
        let mut config = Config::default();
        config.workflows.push(WorkflowRecord::default());
        assert_eq!(config.workflow_index(1).unwrap(), 0);
        assert!(config.workflow_index(0).is_err());
        assert!(config.workflow_index(2).is_err());
    }

    #[test]
    fn defaults_my_ini_to_the_installation_root() {
        let instance = &sample().instances[0];
        assert_eq!(
            instance.my_ini_path(),
            PathBuf::from(r"C:\mysql\5.6").join("my.ini")
        );
        assert_eq!(
            instance.mysqld_exe(),
            PathBuf::from(r"C:\mysql\5.6")
                .join("bin")
                .join("mysqld.exe")
        );
    }

    #[test]
    fn missing_optional_fields_are_tolerated() {
        let config: Config =
            toml::from_str("[[instances]]\nname = \"a\"\nhome = \"C:/a\"\n").unwrap();
        assert_eq!(config.instances[0].root_password, "");
        assert_eq!(config.instances[0].last_pid, None);
        assert!(config.instances[0].my_ini.is_none());
    }

    #[test]
    fn require_reports_the_registration_hint() {
        let config = Config::default();
        let error = config.require("ghost").unwrap_err();
        assert!(error.to_string().contains("ghost"));
        assert!(error.to_string().contains("instance add"));
    }
}
