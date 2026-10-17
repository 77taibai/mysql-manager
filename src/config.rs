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
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub instances: Vec<Instance>,
    #[serde(default)]
    pub bindings: Vec<Binding>,
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
                "instance `{name}` is not registered; run `mm instance add {name} --home <path>`"
            ))
        })
    }
}

/// Resolves `config.toml` next to the running executable, not the current
/// working directory, so `cargo run` and a direct launch agree.
pub fn config_path() -> Result<PathBuf> {
    let exe = std::env::current_exe()
        .map_err(|error| AppError::general(format!("cannot locate the executable: {error}")))?;
    let dir = exe.parent().ok_or_else(|| {
        AppError::general("the executable path has no parent directory".to_string())
    })?;
    Ok(dir.join(CONFIG_FILE_NAME))
}

pub fn load_from(path: &Path) -> Result<Config> {
    if !path.exists() {
        return Ok(Config::default());
    }
    let text = std::fs::read_to_string(path)
        .map_err(|error| AppError::general(format!("cannot read {}: {error}", path.display())))?;
    if text.trim().is_empty() {
        return Ok(Config::default());
    }
    toml::from_str(&text)
        .map_err(|error| AppError::general(format!("cannot parse {}: {error}", path.display())))
}

pub fn save_to(path: &Path, config: &Config) -> Result<()> {
    let text = toml::to_string_pretty(config).map_err(|error| {
        AppError::general(format!("cannot serialize the configuration: {error}"))
    })?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            AppError::general(format!("cannot create {}: {error}", parent.display()))
        })?;
    }
    // Write to a sibling temporary file first so a crash cannot leave a
    // half-written configuration behind.
    let temp = path.with_extension("toml.tmp");
    std::fs::write(&temp, text)
        .map_err(|error| AppError::general(format!("cannot write {}: {error}", temp.display())))?;
    std::fs::rename(&temp, path).map_err(|error| {
        let _ = std::fs::remove_file(&temp);
        AppError::general(format!("cannot replace {}: {error}", path.display()))
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
            workflow: Some(WorkflowRecord {
                last_run: "2026-02-14T10:00:00+08:00".to_string(),
                instance: "mysql56".to_string(),
                database: "db_abc123".to_string(),
                username: "u_abc123".to_string(),
                password: "pw".to_string(),
            }),
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
        assert_eq!(loaded.workflow.as_ref().unwrap().database, "db_abc123");

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
        assert!(config.workflow.is_none());
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
