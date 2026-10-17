use crate::error::{AppError, Result};
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    pub success: bool,
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl CommandOutput {
    pub fn combined(&self) -> String {
        let stdout = self.stdout.trim();
        let stderr = self.stderr.trim();
        match (stdout.is_empty(), stderr.is_empty()) {
            (false, false) => format!("{stdout}\n{stderr}"),
            (false, true) => stdout.to_string(),
            (true, false) => stderr.to_string(),
            (true, true) => String::new(),
        }
    }
}

/// Abstracts process execution so command construction and error propagation
/// can be unit tested without a real MySQL installation.
pub trait Runner {
    fn run(&self, program: &Path, args: &[String]) -> Result<CommandOutput>;

    /// Runs a program and writes `stdin_data` to its standard input, so SQL
    /// containing secrets never appears in the process command line.
    fn run_with_stdin(
        &self,
        program: &Path,
        args: &[String],
        stdin_data: &str,
    ) -> Result<CommandOutput>;

    /// Starts a server process that outlives this one and returns its PID.
    fn spawn_detached(&self, program: &Path, args: &[String]) -> Result<u32>;

    /// Reports whether a binary is present. Routed through the runner so tests
    /// can exercise command construction without a real installation.
    fn exists(&self, path: &Path) -> bool;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemRunner;

impl Runner for SystemRunner {
    fn run(&self, program: &Path, args: &[String]) -> Result<CommandOutput> {
        let output = Command::new(program).args(args).output().map_err(|error| {
            AppError::mysql(format!("cannot run {}: {error}", program.display()))
        })?;
        Ok(CommandOutput {
            success: output.status.success(),
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        })
    }

    fn run_with_stdin(
        &self,
        program: &Path,
        args: &[String],
        stdin_data: &str,
    ) -> Result<CommandOutput> {
        use std::io::Write;
        let mut child = Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|error| {
                AppError::mysql(format!("cannot run {}: {error}", program.display()))
            })?;

        child
            .stdin
            .take()
            .ok_or_else(|| AppError::mysql("cannot open stdin for mysql.exe".to_string()))?
            .write_all(stdin_data.as_bytes())
            .map_err(|error| {
                AppError::mysql(format!("cannot send SQL to {}: {error}", program.display()))
            })?;

        let output = child.wait_with_output().map_err(|error| {
            AppError::mysql(format!("cannot read {} output: {error}", program.display()))
        })?;
        Ok(CommandOutput {
            success: output.status.success(),
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        })
    }

    fn spawn_detached(&self, program: &Path, args: &[String]) -> Result<u32> {
        spawn_detached(program, args)
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }
}

/// Windows process creation flags: start without a console and in its own
/// process group so the server keeps running after `mm` exits.
#[cfg(windows)]
const DETACHED_PROCESS: u32 = 0x0000_0008;
#[cfg(windows)]
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

fn spawn_detached(program: &Path, args: &[String]) -> Result<u32> {
    let mut command = Command::new(program);
    command.args(args);

    // Discard the server's stdio. mysqld writes its own `.err` log inside the
    // datadir, and letting it inherit this process's stdout would interleave
    // server chatter with `mm`'s own output.
    command.stdin(std::process::Stdio::null());
    command.stdout(std::process::Stdio::null());
    command.stderr(std::process::Stdio::null());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }

    let child = command
        .spawn()
        .map_err(|error| AppError::mysql(format!("cannot start {}: {error}", program.display())))?;
    Ok(child.id())
}

/// Reports whether a PID is still alive, guarding against PID reuse by also
/// comparing the image name.
pub fn process_is_alive(runner: &dyn Runner, pid: u32, expected_image: &str) -> bool {
    let args = vec![
        "/FI".to_string(),
        format!("PID eq {pid}"),
        "/FO".to_string(),
        "CSV".to_string(),
        "/NH".to_string(),
    ];
    let Ok(output) = runner.run(Path::new("tasklist"), &args) else {
        return false;
    };
    if !output.success {
        return false;
    }
    output
        .stdout
        .to_ascii_lowercase()
        .contains(&expected_image.to_ascii_lowercase())
}

/// Force-terminates a PID. Returns whether the kill command reported success.
pub fn force_kill(runner: &dyn Runner, pid: u32) -> bool {
    let args = vec!["/PID".to_string(), pid.to_string(), "/F".to_string()];
    matches!(runner.run(Path::new("taskkill"), &args), Ok(output) if output.success)
}

pub fn sleep_secs(seconds: f32) {
    let duration = std::time::Duration::from_secs_f32(seconds.max(0.0));
    std::thread::sleep(duration);
}

#[cfg(test)]
pub mod testing {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::path::Path;

    #[derive(Debug, Clone)]
    pub struct Invocation {
        pub program: String,
        pub args: Vec<String>,
    }

    /// Records every invocation and replays queued responses in order.
    #[allow(dead_code)]
    pub struct FakeRunner {
        invocations: RefCell<Vec<Invocation>>,
        responses: RefCell<VecDeque<Result<CommandOutput>>>,
        spawns: RefCell<Vec<Invocation>>,
        next_pid: RefCell<u32>,
        missing: RefCell<Vec<String>>,
        stdin: RefCell<Vec<String>>,
    }

    impl FakeRunner {
        pub fn new() -> Self {
            Self {
                invocations: RefCell::new(Vec::new()),
                responses: RefCell::new(VecDeque::new()),
                spawns: RefCell::new(Vec::new()),
                next_pid: RefCell::new(4242),
                missing: RefCell::new(Vec::new()),
                stdin: RefCell::new(Vec::new()),
            }
        }

        /// Marks a binary as absent, so `exists` reports `false` for it.
        pub fn hide_binary(&self, path: &str) -> &Self {
            self.missing.borrow_mut().push(path.to_string());
            self
        }

        pub fn push_ok(&self, stdout: &str) -> &Self {
            self.responses.borrow_mut().push_back(Ok(CommandOutput {
                success: true,
                code: Some(0),
                stdout: stdout.to_string(),
                stderr: String::new(),
            }));
            self
        }

        /// A successful run whose payload arrives on stderr, as mysqld does for
        /// `--version` on some builds.
        pub fn push_ok_stderr(&self, stdout: &str, stderr: &str) -> &Self {
            self.responses.borrow_mut().push_back(Ok(CommandOutput {
                success: true,
                code: Some(0),
                stdout: stdout.to_string(),
                stderr: stderr.to_string(),
            }));
            self
        }

        pub fn push_fail(&self, code: i32, stderr: &str) -> &Self {
            self.responses.borrow_mut().push_back(Ok(CommandOutput {
                success: false,
                code: Some(code),
                stdout: String::new(),
                stderr: stderr.to_string(),
            }));
            self
        }

        pub fn push_error(&self, message: &str) -> &Self {
            self.responses
                .borrow_mut()
                .push_back(Err(AppError::mysql(message.to_string())));
            self
        }

        pub fn last(&self) -> Invocation {
            self.invocations
                .borrow()
                .last()
                .cloned()
                .expect("no invocation was recorded")
        }

        /// Returns everything written to a child's stdin, oldest first.
        pub fn stdin_writes(&self) -> Vec<String> {
            self.stdin.borrow().clone()
        }

        /// Returns every recorded invocation, oldest first.
        pub fn invocations(&self) -> Vec<Invocation> {
            self.invocations.borrow().clone()
        }

        /// Returns every detached spawn request, oldest first.
        pub fn spawns(&self) -> Vec<Invocation> {
            self.spawns.borrow().clone()
        }
    }

    impl Runner for FakeRunner {
        fn run(&self, program: &Path, args: &[String]) -> Result<CommandOutput> {
            self.invocations.borrow_mut().push(Invocation {
                program: program.display().to_string(),
                args: args.to_vec(),
            });
            self.responses
                .borrow_mut()
                .pop_front()
                .unwrap_or(Ok(CommandOutput {
                    success: true,
                    code: Some(0),
                    stdout: String::new(),
                    stderr: String::new(),
                }))
        }

        fn spawn_detached(&self, program: &Path, args: &[String]) -> Result<u32> {
            self.spawns.borrow_mut().push(Invocation {
                program: program.display().to_string(),
                args: args.to_vec(),
            });
            let mut pid = self.next_pid.borrow_mut();
            *pid += 1;
            Ok(*pid)
        }

        fn run_with_stdin(
            &self,
            program: &Path,
            args: &[String],
            stdin_data: &str,
        ) -> Result<CommandOutput> {
            self.stdin.borrow_mut().push(stdin_data.to_string());
            self.run(program, args)
        }

        fn exists(&self, path: &Path) -> bool {
            let target = path.display().to_string();
            !self.missing.borrow().contains(&target)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::FakeRunner;
    use super::*;

    #[test]
    fn combined_output_prefers_whichever_stream_has_text() {
        let only_stderr = CommandOutput {
            success: false,
            code: Some(1),
            stdout: String::new(),
            stderr: "boom".to_string(),
        };
        assert_eq!(only_stderr.combined(), "boom");

        let both = CommandOutput {
            success: false,
            code: Some(1),
            stdout: "out".to_string(),
            stderr: "err".to_string(),
        };
        assert_eq!(both.combined(), "out\nerr");
    }

    #[test]
    fn process_liveness_requires_a_matching_image_name() {
        let runner = FakeRunner::new();
        runner.push_ok("\"mysqld.exe\",\"1234\",\"Console\",\"1\",\"10,000 K\"");
        assert!(process_is_alive(&runner, 1234, "mysqld.exe"));

        let runner = FakeRunner::new();
        runner.push_ok("\"other.exe\",\"1234\",\"Console\",\"1\",\"10,000 K\"");
        assert!(!process_is_alive(&runner, 1234, "mysqld.exe"));
    }

    #[test]
    fn process_liveness_is_false_when_tasklist_fails() {
        let runner = FakeRunner::new();
        runner.push_fail(1, "INFO: No tasks are running");
        assert!(!process_is_alive(&runner, 1234, "mysqld.exe"));

        let runner = FakeRunner::new();
        runner.push_error("tasklist is missing");
        assert!(!process_is_alive(&runner, 1234, "mysqld.exe"));
    }

    #[test]
    fn force_kill_reports_the_command_result() {
        let runner = FakeRunner::new();
        runner.push_ok("SUCCESS: sent termination signal");
        assert!(force_kill(&runner, 99));
        assert_eq!(
            runner.last().args,
            vec!["/PID".to_string(), "99".to_string(), "/F".to_string()]
        );

        let runner = FakeRunner::new();
        runner.push_fail(128, "not found");
        assert!(!force_kill(&runner, 99));
    }
}
