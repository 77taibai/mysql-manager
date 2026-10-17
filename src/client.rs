use crate::config::Instance;
use crate::error::{AppError, Result};
use crate::ini::MysqldSettings;
use crate::process::{CommandOutput, Runner};
use crate::sql::ServerVersion;
use std::path::{Path, PathBuf};

/// Owns a temporary client option file and removes it on drop, so the password
/// never reaches a command line and never lingers on disk.
pub struct CredentialFile {
    path: PathBuf,
}

impl CredentialFile {
    pub fn create(user: &str, password: &str, host: &str, port: u16) -> Result<Self> {
        let unique = format!(
            "mm-client-{}-{}.ini",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|value| value.as_nanos())
                .unwrap_or(0)
        );
        let path = std::env::temp_dir().join(unique);
        let content = format!(
            "[client]\nuser={user}\npassword=\"{}\"\nhost={host}\nport={port}\n",
            escape_option_value(password)
        );
        std::fs::write(&path, content).map_err(|error| {
            AppError::general(format!("cannot create {}: {error}", path.display()))
        })?;
        Ok(Self { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for CredentialFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Double quotes inside an option file value must be escaped.
fn escape_option_value(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

/// A `mysql.exe` client bound to one instance and one set of credentials.
pub struct MysqlClient<'a> {
    runner: &'a dyn Runner,
    instance: &'a Instance,
    settings: &'a MysqldSettings,
    ini_path: &'a Path,
}

impl<'a> MysqlClient<'a> {
    pub fn new(
        runner: &'a dyn Runner,
        instance: &'a Instance,
        settings: &'a MysqldSettings,
        ini_path: &'a Path,
    ) -> Self {
        Self {
            runner,
            instance,
            settings,
            ini_path,
        }
    }

    pub fn port(&self) -> Result<u16> {
        self.settings.require_port(self.ini_path)
    }

    fn run_as(&self, user: &str, password: &str, extra: &[String]) -> Result<CommandOutput> {
        let port = self.port()?;
        let credentials = CredentialFile::create(user, password, "127.0.0.1", port)?;
        let mut args = vec![
            format!("--defaults-file={}", credentials.path().display()),
            "--batch".to_string(),
            "--skip-column-names".to_string(),
            "--connect-timeout=10".to_string(),
        ];
        args.extend_from_slice(extra);
        let program = self.instance.mysql_exe();
        if !self.runner.exists(&program) {
            return Err(AppError::instance(format!(
                "mysql.exe was not found at {}",
                program.display()
            )));
        }
        // `credentials` is dropped at the end of this scope, deleting the file.
        self.runner.run(&program, &args)
    }

    /// Runs one or more statements as root. The script is fed through stdin
    /// rather than `-e`, so a statement containing a password never appears in
    /// the process command line.
    pub fn run_sql(&self, statements: &[String]) -> Result<String> {
        if statements.is_empty() {
            return Ok(String::new());
        }
        let script = format!("{};\n", statements.join(";\n"));
        let port = self.port()?;
        let credentials =
            CredentialFile::create("root", &self.instance.root_password, "127.0.0.1", port)?;
        let args = vec![
            format!("--defaults-file={}", credentials.path().display()),
            "--batch".to_string(),
            "--skip-column-names".to_string(),
            "--connect-timeout=10".to_string(),
        ];
        let program = self.instance.mysql_exe();
        if !self.runner.exists(&program) {
            return Err(AppError::instance(format!(
                "mysql.exe was not found at {}",
                program.display()
            )));
        }
        let output = self.runner.run_with_stdin(&program, &args, &script)?;
        if !output.success {
            let detail = output.combined();
            return Err(AppError::mysql(if detail.is_empty() {
                format!("mysql.exe exited with status {:?}", output.code)
            } else {
                detail
            }));
        }
        Ok(output.stdout.trim().to_string())
    }

    /// Runs a query as root and returns the rows, one per line.
    pub fn query(&self, sql: &str) -> Result<Vec<String>> {
        let raw = self.run_sql(&[sql.to_string()])?;
        Ok(raw
            .lines()
            .map(|line| line.trim_end_matches('\r').to_string())
            .filter(|line| !line.is_empty())
            .collect())
    }

    /// Returns `true` when the server answers. Never fails: an unreachable
    /// server is simply not running.
    pub fn is_reachable(&self) -> bool {
        let Ok(port) = self.port() else {
            return false;
        };
        let Ok(credentials) =
            CredentialFile::create("root", &self.instance.root_password, "127.0.0.1", port)
        else {
            return false;
        };
        let args = vec![
            format!("--defaults-file={}", credentials.path().display()),
            "--connect-timeout=3".to_string(),
            "ping".to_string(),
        ];
        let program = self.instance.mysqladmin_exe();
        if !self.runner.exists(&program) {
            return false;
        }
        matches!(self.runner.run(&program, &args), Ok(output) if output.success)
    }

    /// Asks the server to shut down. Returns whether it accepted the request.
    pub fn shutdown(&self) -> bool {
        let Ok(port) = self.port() else {
            return false;
        };
        let Ok(credentials) =
            CredentialFile::create("root", &self.instance.root_password, "127.0.0.1", port)
        else {
            return false;
        };
        let args = vec![
            format!("--defaults-file={}", credentials.path().display()),
            "--connect-timeout=5".to_string(),
            "shutdown".to_string(),
        ];
        let program = self.instance.mysqladmin_exe();
        if !self.runner.exists(&program) {
            return false;
        }
        matches!(self.runner.run(&program, &args), Ok(output) if output.success)
    }

    /// Verifies that a freshly created account can actually connect to its own
    /// schema.
    pub fn verify_account(&self, user: &str, password: &str, database: &str) -> Result<()> {
        let extra = vec![
            format!("--database={database}"),
            "-e".to_string(),
            "SELECT 1".to_string(),
        ];
        let output = self.run_as(user, password, &extra)?;
        if output.success {
            Ok(())
        } else {
            Err(AppError::mysql(format!(
                "the new account `{user}` could not connect to `{database}`: {}",
                output.combined()
            )))
        }
    }

    /// Reads `SHOW GRANTS` for an account.
    pub fn show_grants(&self, user: &str, host: &str) -> Result<Vec<String>> {
        self.query(&format!(
            "SHOW GRANTS FOR {}@{}",
            crate::sql::quote_literal(user),
            crate::sql::quote_literal(host)
        ))
    }

    /// Detects the server version by asking `mysqld --version`, which works
    /// even while the server is stopped.
    pub fn detect_version(&self) -> Result<ServerVersion> {
        let program = self.instance.mysqld_exe();
        if !self.runner.exists(&program) {
            return Err(AppError::instance(format!(
                "mysqld.exe was not found at {}",
                program.display()
            )));
        }
        let args = vec!["--version".to_string()];
        let output = self.runner.run(&program, &args)?;
        let text = if output.stdout.trim().is_empty() {
            output.stderr.clone()
        } else {
            output.stdout.clone()
        };
        ServerVersion::parse(&text).ok_or_else(|| {
            AppError::instance(format!(
                "cannot determine the MySQL version from `{}` output: {}",
                program.display(),
                text.trim()
            ))
        })
    }

    /// Lists existing account names as `user@host`.
    pub fn existing_accounts(&self) -> Result<Vec<String>> {
        self.query("SELECT CONCAT(User, '@', Host) FROM mysql.user")
    }

    /// Lists existing schema names.
    pub fn existing_schemas(&self) -> Result<Vec<String>> {
        self.query("SHOW DATABASES")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::testing::FakeRunner;

    fn instance() -> Instance {
        Instance {
            name: "mysql56".to_string(),
            home: r"C:\mysql\56".to_string(),
            my_ini: None,
            root_password: "rootpw".to_string(),
            last_pid: None,
        }
    }

    fn settings() -> MysqldSettings {
        MysqldSettings {
            basedir: None,
            datadir: Some("C:/mysql/56/data".to_string()),
            port: Some(3306),
        }
    }

    fn ini() -> PathBuf {
        PathBuf::from(r"C:\mysql\56\my.ini")
    }

    #[test]
    fn credential_file_is_removed_on_drop() {
        let path = {
            let file = CredentialFile::create("root", "pw", "127.0.0.1", 3306).unwrap();
            assert!(file.path().exists());
            file.path().to_path_buf()
        };
        assert!(
            !path.exists(),
            "the credential file must be deleted on drop"
        );
    }

    #[test]
    fn credential_file_escapes_quotes_in_the_password() {
        let file = CredentialFile::create("root", r#"p"w\d"#, "127.0.0.1", 3306).unwrap();
        let content = std::fs::read_to_string(file.path()).unwrap();
        assert!(content.contains(r#"password="p\"w\\d""#), "got: {content}");
    }

    #[test]
    fn sql_is_passed_through_a_defaults_file_not_the_command_line() {
        let runner = FakeRunner::new();
        runner.push_ok("");
        let inst = instance();
        let ini_path = ini();
        let cfg = settings();
        let client = MysqlClient::new(&runner, &inst, &cfg, &ini_path);

        client
            .run_sql(&["CREATE DATABASE `x`".to_string()])
            .unwrap();

        let invocation = runner.last();
        assert!(invocation.program.ends_with("mysql.exe"));
        let joined = invocation.args.join(" ");
        assert!(joined.contains("--defaults-file="));
        // `query()` relies on these to get one bare value per line.
        assert!(
            invocation.args.iter().any(|arg| arg == "--batch"),
            "--batch is required for machine-readable output"
        );
        assert!(
            invocation
                .args
                .iter()
                .any(|arg| arg == "--skip-column-names"),
            "--skip-column-names is required for machine-readable output"
        );
        assert!(
            !joined.contains("rootpw"),
            "the root password must never appear on the command line"
        );
        // The statement itself travels on stdin, not in argv.
        assert!(
            !joined.contains("CREATE DATABASE"),
            "SQL must not be in argv: {joined}"
        );
        assert_eq!(
            runner.stdin_writes(),
            vec!["CREATE DATABASE `x`;\n".to_string()]
        );
    }

    #[test]
    fn a_created_account_password_never_appears_in_argv() {
        // This is the case the defaults-file alone does not cover: the new
        // account's password is part of the DDL text itself.
        let runner = FakeRunner::new();
        runner.push_ok("");
        let inst = instance();
        let ini_path = ini();
        let cfg = settings();
        let client = MysqlClient::new(&runner, &inst, &cfg, &ini_path);

        let plan = crate::sql::create_user_sql(
            crate::sql::ServerVersion {
                major: 5,
                minor: 6,
                patch: 51,
            },
            "app",
            "localhost",
            "sup3rSecret",
        )
        .unwrap();
        client.run_sql(&plan.statements).unwrap();

        let joined = runner.last().args.join(" ");
        assert!(
            !joined.contains("sup3rSecret"),
            "the new account password leaked into argv: {joined}"
        );
        assert!(runner.stdin_writes()[0].contains("sup3rSecret"));
    }

    #[test]
    fn sql_failures_surface_the_server_message_verbatim() {
        let runner = FakeRunner::new();
        runner.push_fail(1, "ERROR 1396 (HY000): Operation CREATE USER failed");
        let inst = instance();
        let ini_path = ini();
        let cfg = settings();
        let client = MysqlClient::new(&runner, &inst, &cfg, &ini_path);

        let error = client.run_sql(&["CREATE USER x".to_string()]).unwrap_err();
        assert!(error.to_string().contains("ERROR 1396"));
        assert_eq!(error.code(), crate::error::EXIT_MYSQL);
    }

    #[test]
    fn a_rejected_script_still_reports_the_server_error_not_a_pipe_error() {
        // The child may close stdin before the whole script is written. The
        // reported failure must be the server's, never a broken-pipe message.
        let runner = FakeRunner::new();
        runner.push_fail(1045, "ERROR 1045 (28000): Access denied for user 'root'");
        let inst = instance();
        let ini_path = ini();
        let cfg = settings();
        let client = MysqlClient::new(&runner, &inst, &cfg, &ini_path);

        let error = client
            .run_sql(&["CREATE USER 'a'@'localhost' IDENTIFIED BY 'pw'".to_string()])
            .unwrap_err();
        let text = error.to_string();
        assert!(text.contains("ERROR 1045"), "got: {text}");
        assert!(!text.contains("cannot send SQL"), "got: {text}");
    }

    #[test]
    fn an_unreachable_server_is_not_an_error_for_ping() {
        let runner = FakeRunner::new();
        runner.push_fail(1, "mysqld is not running");
        let inst = instance();
        let ini_path = ini();
        let cfg = settings();
        let client = MysqlClient::new(&runner, &inst, &cfg, &ini_path);
        assert!(!client.is_reachable());

        let runner = FakeRunner::new();
        runner.push_error("cannot spawn");
        let client = MysqlClient::new(&runner, &inst, &cfg, &ini_path);
        assert!(!client.is_reachable());
    }

    #[test]
    fn query_splits_rows_and_drops_blank_lines() {
        let runner = FakeRunner::new();
        runner.push_ok("mysql\nsys\n\nappdb\n");
        let inst = instance();
        let ini_path = ini();
        let cfg = settings();
        let client = MysqlClient::new(&runner, &inst, &cfg, &ini_path);
        assert_eq!(
            client.query("SHOW DATABASES").unwrap(),
            vec!["mysql", "sys", "appdb"]
        );
    }

    #[test]
    fn version_detection_uses_mysqld_version_output() {
        let runner = FakeRunner::new();
        runner.push_ok("C:\\mysql\\bin\\mysqld.exe  Ver 5.6.51 for Win64 on x86_64");
        let inst = instance();
        let ini_path = ini();
        let cfg = settings();
        let client = MysqlClient::new(&runner, &inst, &cfg, &ini_path);
        let version = client.detect_version().unwrap();
        assert_eq!(version.major, 5);
        assert_eq!(version.minor, 6);
        assert_eq!(version.patch, 51);
    }

    #[test]
    fn version_detection_falls_back_to_stderr() {
        let runner = FakeRunner::new();
        runner.push_ok_stderr("", "Ver 5.7.44 for Win64 on x86_64");
        let inst = instance();
        let ini_path = ini();
        let cfg = settings();
        let client = MysqlClient::new(&runner, &inst, &cfg, &ini_path);
        let version = client.detect_version().unwrap();
        assert_eq!(version.major, 5);
        assert_eq!(version.minor, 7);
        assert_eq!(version.patch, 44);
    }

    #[test]
    fn version_detection_fails_cleanly_when_no_version_is_present() {
        let runner = FakeRunner::new();
        runner.push_ok("no version here");
        let inst = instance();
        let ini_path = ini();
        let cfg = settings();
        let client = MysqlClient::new(&runner, &inst, &cfg, &ini_path);
        let error = client.detect_version().unwrap_err();
        assert!(error
            .to_string()
            .contains("cannot determine the MySQL version"));
    }

    #[test]
    fn verifying_an_account_reports_a_clear_failure() {
        let runner = FakeRunner::new();
        runner.push_fail(1045, "ERROR 1045 (28000): Access denied");
        let inst = instance();
        let ini_path = ini();
        let cfg = settings();
        let client = MysqlClient::new(&runner, &inst, &cfg, &ini_path);
        let error = client.verify_account("u_x", "pw", "db_x").unwrap_err();
        assert!(error.to_string().contains("could not connect"));
        assert!(error.to_string().contains("ERROR 1045"));
    }
}
