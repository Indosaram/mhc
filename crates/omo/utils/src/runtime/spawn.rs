use std::collections::HashMap;
use std::io::{self, Read};
use std::path::PathBuf;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StdioMode {
    Pipe,
    Inherit,
    Ignore,
}

impl StdioMode {
    fn to_stdio(self) -> Stdio {
        match self {
            StdioMode::Pipe => Stdio::piped(),
            StdioMode::Inherit => Stdio::inherit(),
            StdioMode::Ignore => Stdio::null(),
        }
    }
}

/// Spawn options; unset stdio defaults to stdin=ignore, stdout=pipe, stderr=inherit.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpawnOptions {
    pub cwd: Option<PathBuf>,
    /// Replaces the whole child environment when set.
    pub env: Option<HashMap<String, String>>,
    pub stdin: Option<StdioMode>,
    pub stdout: Option<StdioMode>,
    pub stderr: Option<StdioMode>,
    pub stdio: Option<[StdioMode; 3]>,
    pub detached: bool,
}

impl SpawnOptions {
    fn resolved_stdio(&self) -> [StdioMode; 3] {
        self.stdio.unwrap_or([
            self.stdin.unwrap_or(StdioMode::Ignore),
            self.stdout.unwrap_or(StdioMode::Pipe),
            self.stderr.unwrap_or(StdioMode::Inherit),
        ])
    }
}

fn build_command(cmd: &[String], options: &SpawnOptions) -> io::Result<Command> {
    let (bin, args) = cmd
        .split_first()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "spawn requires a command"))?;
    let mut command = Command::new(bin);
    command.args(args);
    if let Some(cwd) = &options.cwd {
        command.current_dir(cwd);
    }
    if let Some(env) = &options.env {
        command.env_clear().envs(env);
    }
    let [stdin, stdout, stderr] = options.resolved_stdio();
    command
        .stdin(stdin.to_stdio())
        .stdout(stdout.to_stdio())
        .stderr(stderr.to_stdio());
    #[cfg(unix)]
    if options.detached {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    Ok(command)
}

/// A running child. Streams are `None` unless piped.
pub struct SpawnedProcess {
    child: Child,
    exit_code: Option<i32>,
    pub stdin: Option<ChildStdin>,
    pub stdout: Option<ChildStdout>,
    pub stderr: Option<ChildStderr>,
}

fn code_of(status: ExitStatus) -> i32 {
    status.code().unwrap_or(1)
}

impl SpawnedProcess {
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn exit_code(&self) -> Option<i32> {
        self.exit_code
    }

    /// Wait for exit; the exit code is cached for [`Self::exit_code`].
    pub fn exited(&mut self) -> io::Result<i32> {
        drop(self.stdin.take());
        let code = code_of(self.child.wait()?);
        self.exit_code = Some(code);
        Ok(code)
    }

    /// Drain stdout and stderr fully, then wait (avoids pipe deadlock).
    pub fn wait_with_output(mut self) -> io::Result<(i32, String, String)> {
        drop(self.stdin.take());
        let stdout = self.stdout.take();
        let stderr = self.stderr.take();
        let err_reader = std::thread::spawn(move || read_all(stderr));
        let out = read_all(stdout);
        let err = err_reader.join().unwrap_or_default();
        let code = code_of(self.child.wait()?);
        Ok((code, out, err))
    }

    pub fn kill(&mut self) {
        if self.exit_code.is_none() {
            let _ = self.child.kill();
        }
    }
}

fn read_all(stream: Option<impl Read>) -> String {
    let mut buffer = Vec::new();
    if let Some(mut stream) = stream {
        let _ = stream.read_to_end(&mut buffer);
    }
    String::from_utf8_lossy(&buffer).into_owned()
}

pub fn spawn(cmd: &[String], options: &SpawnOptions) -> io::Result<SpawnedProcess> {
    let mut child = build_command(cmd, options)?.spawn()?;
    Ok(SpawnedProcess {
        stdin: child.stdin.take(),
        stdout: child.stdout.take(),
        stderr: child.stderr.take(),
        child,
        exit_code: None,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnSyncResult {
    pub exit_code: i32,
    pub stdout: Option<Vec<u8>>,
    pub stderr: Option<Vec<u8>>,
    pub success: bool,
    pub pid: i64,
}

pub fn spawn_sync(cmd: &[String], options: &SpawnOptions) -> io::Result<SpawnSyncResult> {
    let [_, stdout_mode, stderr_mode] = options.resolved_stdio();
    let child = build_command(cmd, options)?.spawn()?;
    let pid = i64::from(child.id());
    let output = child.wait_with_output()?;
    let exit_code = code_of(output.status);
    Ok(SpawnSyncResult {
        exit_code,
        stdout: (stdout_mode == StdioMode::Pipe).then_some(output.stdout),
        stderr: (stderr_mode == StdioMode::Pipe).then_some(output.stderr),
        success: exit_code == 0,
        pid,
    })
}
