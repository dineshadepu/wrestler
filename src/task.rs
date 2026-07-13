use anyhow::{Context as _, Result};
use std::{
    io::{Read, Write},
    path::PathBuf,
    process::{Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

pub struct TaskResult {
    pub duration: Duration,
    pub status: ExitStatus,
    pub stdout: String,
    pub stderr: String,
}

impl TaskResult {
    pub fn success(&self) -> bool {
        self.status.success()
    }
}

/// Copy `reader` to `writer` as it arrives, keeping a copy of everything read.
fn tee(mut reader: impl Read, mut writer: impl Write) -> std::io::Result<Vec<u8>> {
    let mut captured = Vec::new();
    let mut buf = [0u8; 8192];

    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        writer.write_all(&buf[..n])?;
        writer.flush()?;
        captured.extend_from_slice(&buf[..n]);
    }

    Ok(captured)
}

#[derive(Clone, Debug)]
pub struct Task {
    pub name: String,
    pub executable: PathBuf,
    pub working_directory: PathBuf,
    pub args: Vec<String>,
}

impl Task {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            executable: PathBuf::new(),
            working_directory: PathBuf::new(),
            args: Vec::new(),
        }
    }

    pub fn executable(mut self, exe: impl Into<PathBuf>) -> Self {
        self.executable = exe.into();
        self
    }

    pub fn working_directory(mut self, dir: impl Into<PathBuf>) -> Self {
        self.working_directory = dir.into();
        self
    }

    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn to_shell(&self) -> String {
        let mut s = String::new();

        if self.working_directory.as_os_str().is_empty() {
            s.push_str("cd \"$ROOT\"\n");
        } else if self.working_directory.is_absolute() {
            s.push_str(&format!("cd {}\n", self.working_directory.display()));
        } else {
            s.push_str(&format!("cd \"$ROOT\"/{}\n", self.working_directory.display()));
        }

        s.push_str(&self.executable.display().to_string());

        for arg in &self.args {
            s.push(' ');
            s.push_str(arg);
        }

        s.push('\n');

        s
    }

    /// Run the task, streaming stdout/stderr to the terminal while
    /// capturing them for the [`TaskResult`].
    ///
    /// A non-zero exit status is not an `Err`: the result records it and
    /// the caller decides whether to stop. `Err` means the process could
    /// not be run at all.
    pub fn execute(&self) -> Result<TaskResult> {
        println!("--------------------------------");
        println!("{}", self.name);
        println!("{}", self.executable.display());

        let start = Instant::now();

        let mut command = Command::new(&self.executable);
        command.args(&self.args);
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());

        if !self.working_directory.as_os_str().is_empty() {
            command.current_dir(&self.working_directory);
        }

        let mut child = command
            .spawn()
            .with_context(|| format!("Task '{}': failed to start {}", self.name, self.executable.display()))?;

        let child_stdout = child.stdout.take().expect("stdout was piped");
        let child_stderr = child.stderr.take().expect("stderr was piped");

        let stdout_thread = thread::spawn(move || tee(child_stdout, std::io::stdout()));
        let stderr_thread = thread::spawn(move || tee(child_stderr, std::io::stderr()));

        let status = child.wait()?;

        let stdout = stdout_thread.join().expect("stdout tee thread panicked")?;
        let stderr = stderr_thread.join().expect("stderr tee thread panicked")?;

        let duration = start.elapsed();

        Ok(TaskResult {
            duration,
            status,
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_shell_relative_directory_is_anchored_to_root() {
        let task = Task::new("Run Solver")
            .executable("./solver")
            .working_directory("outputs/dam_break")
            .arg("--dx=0.001");

        assert_eq!(task.to_shell(), "cd \"$ROOT\"/outputs/dam_break\n./solver --dx=0.001\n");
    }

    #[test]
    fn to_shell_absolute_directory_is_used_as_is() {
        let task = Task::new("Run Solver")
            .executable("/opt/solver")
            .working_directory("/data/outputs");

        assert_eq!(task.to_shell(), "cd /data/outputs\n/opt/solver\n");
    }

    #[test]
    fn to_shell_empty_directory_returns_to_root() {
        let task = Task::new("List").executable("ls");

        assert_eq!(task.to_shell(), "cd \"$ROOT\"\nls\n");
    }
}
