use crate::client::MysqlClient;
use crate::config::Instance;
use crate::error::{AppError, Result};
use crate::ini::MysqldSettings;
use crate::process::{self, Runner};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Running,
    Stopped,
}

impl Status {
    pub fn label(&self) -> &'static str {
        match self {
            Status::Running => "Running",
            Status::Stopped => "Stopped",
        }
    }
}

pub struct Lifecycle<'a> {
    runner: &'a dyn Runner,
    /// Polling budget in iterations; each iteration sleeps `poll_interval`.
    pub ready_attempts: u32,
    pub poll_interval_secs: f32,
}

impl<'a> Lifecycle<'a> {
    pub fn new(runner: &'a dyn Runner) -> Self {
        Self {
            runner,
            ready_attempts: 60,
            poll_interval_secs: 0.5,
        }
    }

    #[cfg(test)]
    pub fn with_budget(mut self, attempts: u32, interval_secs: f32) -> Self {
        self.ready_attempts = attempts;
        self.poll_interval_secs = interval_secs;
        self
    }

    fn client<'b>(
        &'b self,
        instance: &'b Instance,
        settings: &'b MysqldSettings,
        ini: &'b Path,
    ) -> MysqlClient<'b> {
        MysqlClient::new(self.runner, instance, settings, ini)
    }

    /// Determines the live status. A stale PID (reused by another process) is
    /// never treated as a running server.
    pub fn status(
        &self,
        instance: &Instance,
        settings: &MysqldSettings,
        ini: &Path,
    ) -> Result<Status> {
        let client = self.client(instance, settings, ini);
        if client.is_reachable() {
            return Ok(Status::Running);
        }
        Ok(Status::Stopped)
    }

    /// Reports whether a recorded PID still belongs to a `mysqld` process.
    pub fn recorded_pid_is_live(&self, instance: &Instance) -> bool {
        match instance.last_pid {
            Some(pid) => process::process_is_alive(self.runner, pid, "mysqld.exe"),
            None => false,
        }
    }

    /// Starts the server in the background and waits until it answers.
    pub fn start(
        &mut self,
        instance: &Instance,
        settings: &MysqldSettings,
        ini: &Path,
    ) -> Result<u32> {
        let client = self.client(instance, settings, ini);
        if client.is_reachable() {
            return Err(AppError::mysql(format!(
                "instance `{}` is already running on port {}",
                instance.name,
                client.port()?
            )));
        }

        let mysqld = instance.mysqld_exe();
        if !self.runner.exists(&mysqld) {
            return Err(AppError::instance(format!(
                "mysqld.exe was not found at {}",
                mysqld.display()
            )));
        }
        let datadir = settings.require_datadir(ini)?;
        if !datadir.exists() {
            return Err(AppError::instance(format!(
                "the datadir configured in {} does not exist: {}",
                ini.display(),
                datadir.display()
            )));
        }

        let args = vec![format!("--defaults-file={}", ini.display())];
        let pid = self.runner.spawn_detached(&mysqld, &args)?;

        for _ in 0..self.ready_attempts {
            if self.client(instance, settings, ini).is_reachable() {
                return Ok(pid);
            }
            if !process::process_is_alive(self.runner, pid, "mysqld.exe") {
                return Err(AppError::mysql(format!(
                    "mysqld exited during startup; check the error log at {}",
                    error_log_hint(&datadir).display()
                )));
            }
            process::sleep_secs(self.poll_interval_secs);
        }

        // Startup timed out: do not leave an unresponsive server behind.
        process::force_kill(self.runner, pid);
        Err(AppError::mysql(format!(
            "instance `{}` did not become ready within {} seconds; check the error log at {}",
            instance.name,
            (self.ready_attempts as f32 * self.poll_interval_secs) as u32,
            error_log_hint(&datadir).display()
        )))
    }

    /// Stops the server, preferring a clean shutdown and falling back to a
    /// forced termination of the recorded PID.
    pub fn stop(
        &mut self,
        instance: &Instance,
        settings: &MysqldSettings,
        ini: &Path,
    ) -> Result<()> {
        let client = self.client(instance, settings, ini);
        let reachable = client.is_reachable();
        let pid_live = self.recorded_pid_is_live(instance);

        if !reachable && !pid_live {
            return Err(AppError::mysql(format!(
                "instance `{}` is not running",
                instance.name
            )));
        }

        if reachable && client.shutdown() {
            for _ in 0..self.ready_attempts {
                if !self.client(instance, settings, ini).is_reachable() {
                    return Ok(());
                }
                process::sleep_secs(self.poll_interval_secs);
            }
        }

        if let Some(pid) = instance.last_pid {
            if process::force_kill(self.runner, pid) {
                return Ok(());
            }
        }

        if !self.client(instance, settings, ini).is_reachable() {
            return Ok(());
        }

        Err(AppError::mysql(format!(
            "instance `{}` could not be stopped; the server may still be shutting down",
            instance.name
        )))
    }
}

/// Best-effort location of the server error log: the newest `*.err` file in
/// the datadir, falling back to the datadir itself.
pub fn error_log_hint(datadir: &Path) -> PathBuf {
    let Ok(entries) = std::fs::read_dir(datadir) else {
        return datadir.to_path_buf();
    };
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        let is_err = path
            .extension()
            .and_then(|value| value.to_str())
            .map(|value| value.eq_ignore_ascii_case("err"))
            .unwrap_or(false);
        if !is_err {
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        if newest.as_ref().map(|(time, _)| modified > *time).unwrap_or(true) {
            newest = Some((modified, path));
        }
    }
    newest
        .map(|(_, path)| path)
        .unwrap_or_else(|| datadir.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::testing::FakeRunner;

    fn instance(pid: Option<u32>) -> Instance {
        Instance {
            name: "mysql56".to_string(),
            home: r"C:\mysql\56".to_string(),
            my_ini: None,
            root_password: "pw".to_string(),
            last_pid: pid,
        }
    }

    fn settings() -> MysqldSettings {
        MysqldSettings {
            basedir: None,
            datadir: Some(std::env::temp_dir().display().to_string()),
            port: Some(3306),
        }
    }

    fn ini() -> PathBuf {
        PathBuf::from(r"C:\mysql\56\my.ini")
    }

    #[test]
    fn start_refuses_when_the_server_already_answers() {
        let runner = FakeRunner::new();
        runner.push_ok("mysqld is alive");
        let mut lifecycle = Lifecycle::new(&runner).with_budget(1, 0.0);
        let error = lifecycle
            .start(&instance(None), &settings(), &ini())
            .unwrap_err();
        assert!(error.to_string().contains("already running"));
    }

    #[test]
    fn start_reports_a_missing_mysqld_binary() {
        let runner = FakeRunner::new();
        runner.push_fail(1, "not running");
        runner.hide_binary(r"C:\mysql\56\bin\mysqld.exe");
        let mut lifecycle = Lifecycle::new(&runner).with_budget(1, 0.0);
        let error = lifecycle
            .start(&instance(None), &settings(), &ini())
            .unwrap_err();
        assert!(error.to_string().contains("mysqld.exe was not found"));
        assert_eq!(error.code(), crate::error::EXIT_INSTANCE);
    }

    #[test]
    fn start_requires_the_configured_datadir_to_exist() {
        let runner = FakeRunner::new();
        runner.push_fail(1, "not running");
        let missing = MysqldSettings {
            basedir: None,
            datadir: Some(r"C:\definitely\not\here".to_string()),
            port: Some(3306),
        };
        let mut lifecycle = Lifecycle::new(&runner).with_budget(1, 0.0);
        let error = lifecycle
            .start(&instance(None), &missing, &ini())
            .unwrap_err();
        assert!(error.to_string().contains("does not exist"));
    }

    #[test]
    fn stop_refuses_when_nothing_is_running() {
        let runner = FakeRunner::new();
        runner.push_fail(1, "not running");
        let mut lifecycle = Lifecycle::new(&runner).with_budget(1, 0.0);
        let error = lifecycle
            .stop(&instance(None), &settings(), &ini())
            .unwrap_err();
        assert!(error.to_string().contains("is not running"));
    }

    #[test]
    fn stop_falls_back_to_killing_the_recorded_pid() {
        // ping fails -> unreachable; tasklist reports the pid alive; shutdown is
        // never attempted because the server is unreachable; taskkill succeeds.
        let runner = FakeRunner::new();
        runner.push_fail(1, "unreachable"); // is_reachable
        runner.push_ok("\"mysqld.exe\",\"777\",\"Console\",\"1\",\"10 K\""); // pid alive
        runner.push_ok("SUCCESS"); // taskkill
        let mut lifecycle = Lifecycle::new(&runner).with_budget(1, 0.0);
        lifecycle
            .stop(&instance(Some(777)), &settings(), &ini())
            .expect("stop should succeed via taskkill");
    }

    #[test]
    fn stale_pid_of_another_process_is_not_treated_as_running() {
        let runner = FakeRunner::new();
        runner.push_fail(1, "unreachable"); // ping
        runner.push_ok("\"chrome.exe\",\"777\",\"Console\",\"1\",\"10 K\""); // wrong image
        let mut lifecycle = Lifecycle::new(&runner).with_budget(1, 0.0);
        let error = lifecycle
            .stop(&instance(Some(777)), &settings(), &ini())
            .unwrap_err();
        assert!(error.to_string().contains("is not running"));
    }

    #[test]
    fn status_reports_running_when_ping_succeeds() {
        let runner = FakeRunner::new();
        runner.push_ok("mysqld is alive");
        let lifecycle = Lifecycle::new(&runner).with_budget(1, 0.0);
        assert_eq!(
            lifecycle.status(&instance(None), &settings(), &ini()).unwrap(),
            Status::Running
        );
    }

    #[test]
    fn status_reports_stopped_when_ping_fails() {
        let runner = FakeRunner::new();
        runner.push_fail(1, "unreachable");
        let lifecycle = Lifecycle::new(&runner).with_budget(1, 0.0);
        assert_eq!(
            lifecycle.status(&instance(None), &settings(), &ini()).unwrap(),
            Status::Stopped
        );
    }

    #[test]
    fn error_log_hint_falls_back_to_the_datadir() {
        let missing = PathBuf::from(r"C:\definitely\not\here");
        assert_eq!(error_log_hint(&missing), missing);
    }

    #[test]
    fn error_log_hint_finds_the_newest_err_file() {
        let dir = std::env::temp_dir().join(format!("mm-errlog-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("old.err"), "a").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(dir.join("new.err"), "b").unwrap();
        std::fs::write(dir.join("notes.txt"), "c").unwrap();
        let found = error_log_hint(&dir);
        assert_eq!(found.file_name().unwrap(), "new.err");
        std::fs::remove_dir_all(&dir).ok();
    }
}
