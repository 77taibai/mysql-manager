use crate::error::{AppError, Result};
use std::path::{Path, PathBuf};

/// Settings read from the `[mysqld]` section of a MySQL option file.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MysqldSettings {
    pub basedir: Option<String>,
    pub datadir: Option<String>,
    pub port: Option<u16>,
}

impl MysqldSettings {
    pub fn require_datadir(&self, ini: &Path) -> Result<PathBuf> {
        match &self.datadir {
            Some(value) => Ok(PathBuf::from(value)),
            None => Err(AppError::instance(format!(
                "`datadir` is missing from the [mysqld] section of {}",
                ini.display()
            ))),
        }
    }

    pub fn require_port(&self, ini: &Path) -> Result<u16> {
        match self.port {
            Some(port) => Ok(port),
            None => Err(AppError::instance(format!(
                "`port` is missing from the [mysqld] section of {}",
                ini.display()
            ))),
        }
    }
}

/// Strips a `#` or `;` comment. A comment only starts at the beginning of the
/// line or after whitespace, so values such as `C:/a#b` survive intact.
fn strip_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if (*byte == b'#' || *byte == b';')
            && (index == 0 || bytes[index - 1].is_ascii_whitespace())
        {
            return &line[..index];
        }
    }
    line
}

fn unquote(value: &str) -> String {
    let trimmed = value.trim();
    let bytes = trimmed.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return trimmed[1..trimmed.len() - 1].to_string();
        }
    }
    trimmed.to_string()
}

/// Parses an option file and returns the keys we care about from `[mysqld]`.
pub fn parse_mysqld(content: &str) -> MysqldSettings {
    let mut settings = MysqldSettings::default();
    let mut in_mysqld = false;

    for raw in content.lines() {
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            let name = line
                .trim_start_matches('[')
                .split(']')
                .next()
                .unwrap_or("")
                .trim();
            in_mysqld = name.eq_ignore_ascii_case("mysqld");
            continue;
        }
        if !in_mysqld {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = unquote(value);
        match key.trim().to_ascii_lowercase().as_str() {
            "basedir" => settings.basedir = Some(value),
            "datadir" => settings.datadir = Some(value),
            "port" => {
                if let Ok(port) = value.parse::<u16>() {
                    settings.port = Some(port);
                }
            }
            _ => {}
        }
    }

    settings
}

pub fn load(ini: &Path) -> Result<MysqldSettings> {
    let content = std::fs::read_to_string(ini)
        .map_err(|error| AppError::instance(format!("cannot read {}: {error}", ini.display())))?;
    Ok(parse_mysqld(&content))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_local_mysql_56_ini() {
        let content = "[mysqld]\n\
                       basedir=C:/Users/taiba/AppData/Local/Programs/MySQL/mysql-5.6.51-winx64\n\
                       datadir=C:/Users/taiba/AppData/Local/Programs/MySQL/mysql-5.6.51-winx64/mysql-data\n\
                       port=3306\n\
                       sql_mode=NO_ENGINE_SUBSTITUTION,STRICT_TRANS_TABLES\n\
                       \n\
                       [client]\n\
                       port=3307\n";
        let settings = parse_mysqld(content);
        assert_eq!(
            settings.basedir.as_deref(),
            Some("C:/Users/taiba/AppData/Local/Programs/MySQL/mysql-5.6.51-winx64")
        );
        assert_eq!(
            settings.datadir.as_deref(),
            Some("C:/Users/taiba/AppData/Local/Programs/MySQL/mysql-5.6.51-winx64/mysql-data")
        );
        assert_eq!(settings.port, Some(3306));
    }

    #[test]
    fn ignores_keys_outside_the_mysqld_section() {
        let content = "[client]\nport=3399\ndatadir=/nope\n[mysqld]\nport=3310\n";
        let settings = parse_mysqld(content);
        assert_eq!(settings.port, Some(3310));
        assert_eq!(settings.datadir, None);
    }

    #[test]
    fn section_name_is_case_insensitive() {
        let content = "[MySQLD]\ndatadir=C:/data\n";
        assert_eq!(parse_mysqld(content).datadir.as_deref(), Some("C:/data"));
    }

    #[test]
    fn handles_whitespace_quotes_and_comments() {
        let content =
            "[mysqld]\n  datadir = \"C:/my data\"   # trailing note\n; whole line\nport = '3308'\n";
        let settings = parse_mysqld(content);
        assert_eq!(settings.datadir.as_deref(), Some("C:/my data"));
        assert_eq!(settings.port, Some(3308));
    }

    #[test]
    fn keeps_hash_that_is_not_preceded_by_whitespace() {
        let content = "[mysqld]\ndatadir=C:/a#b\n";
        assert_eq!(parse_mysqld(content).datadir.as_deref(), Some("C:/a#b"));
    }

    #[test]
    fn missing_datadir_and_port_are_reported_with_the_ini_path() {
        let settings = parse_mysqld("[mysqld]\nbasedir=C:/x\n");
        let ini = Path::new("C:/x/my.ini");
        let datadir_error = settings.require_datadir(ini).unwrap_err();
        assert!(datadir_error.to_string().contains("datadir"));
        assert!(datadir_error.to_string().contains("C:/x/my.ini"));
        let port_error = settings.require_port(ini).unwrap_err();
        assert!(port_error.to_string().contains("port"));
    }

    #[test]
    fn invalid_port_is_treated_as_missing() {
        let settings = parse_mysqld("[mysqld]\nport=not-a-number\n");
        assert_eq!(settings.port, None);
    }
}
