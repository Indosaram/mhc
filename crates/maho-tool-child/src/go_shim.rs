//! The POSIX go-line shim every own-group launch starts through (senpi#3566 todo 4; review MEDIUM 4). The shim leads the
//! new process group and waits for `go` on a dedicated control descriptor - never stdin, which stays the caller's - so
//! the command runs only after the server recorded the launch. A program that cannot be found is reported back on the
//! same descriptor (`ENOENT`) before the shim exits, so the caller sees the same ENOENT `'error'` as an ungated spawn.
//! The descriptor is closed before the command starts, so the command never sees it.

/// The shell script for a control descriptor `fd` (3 or above). `$0` is the program, `$@` its arguments.
pub fn go_shim_script(fd: usize) -> String {
    [
        format!("IFS= read -r _go <&{fd} || exit 127"),
        format!("command -v \"$0\" >/dev/null 2>&1 || {{ printf 'ENOENT\\n' >&{fd}; exit 127; }}"),
        format!("exec \"$0\" \"$@\" {fd}<&-"),
    ]
    .join("; ")
}

/// The control descriptor for a launch: the first index after every descriptor the caller already asked for.
pub fn control_fd<T>(caller_stdio: &[T]) -> usize {
    caller_stdio.len().max(3)
}

/// The caller's stdio padded to `fd` entries, with the control pipe at `fd`.
pub fn with_control_pipe<T: Clone>(caller_stdio: &[T], fd: usize, pad: T, pipe: T) -> Vec<T> {
    let mut stdio = caller_stdio.to_vec();
    while stdio.len() < fd {
        stdio.push(pad.clone());
    }
    stdio.push(pipe);
    stdio
}

/// The caller's `shell` option: `true`/`false`, or the path of the shell to use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shell {
    Enabled(bool),
    Path(String),
}

/// The program and arguments the shim execs: the command itself, or the caller's shell running it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShimTarget {
    pub program: String,
    pub args: Vec<String>,
}

/// The program and arguments the shim execs: the command itself, or the caller's shell running it.
pub fn shim_target(command: &str, args: &[String], shell: Option<&Shell>) -> ShimTarget {
    match shell {
        None | Some(Shell::Enabled(false)) => ShimTarget {
            program: command.to_string(),
            args: args.to_vec(),
        },
        Some(shell) => {
            let program = match shell {
                Shell::Path(path) => path.clone(),
                Shell::Enabled(_) => "/bin/sh".to_string(),
            };
            let mut line = vec![command.to_string()];
            line.extend(args.iter().cloned());
            ShimTarget {
                program,
                args: vec!["-c".to_string(), line.join(" ")],
            }
        }
    }
}

/// `NodeJS.ErrnoException`: the error an ungated spawn emits for a program it cannot find.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrnoException {
    pub message: String,
    pub errno: i32,
    pub code: String,
    pub syscall: String,
    pub path: String,
    pub spawnargs: Vec<String>,
}

impl std::fmt::Display for ErrnoException {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ErrnoException {}

/// The error an ungated spawn emits for a program it cannot find.
pub fn enoent_error(command: &str, args: &[String]) -> ErrnoException {
    ErrnoException {
        message: format!("spawn {command} ENOENT"),
        errno: -2,
        code: "ENOENT".to_string(),
        syscall: format!("spawn {command}"),
        path: command.to_string(),
        spawnargs: args.to_vec(),
    }
}

/// Creates the go-line control FIFO at `path`, owner read/write only. POSIX `mkfifo` (rustix has no FIFO constructor on
/// Apple targets).
#[cfg(unix)]
#[allow(unsafe_code)]
pub(crate) fn make_fifo(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "fifo path contains NUL")
    })?;
    // SAFETY: `c_path` is a valid NUL-terminated C string that outlives the call; mkfifo only reads it.
    if unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    #[test]
    fn script_reads_the_go_line_then_execs_with_the_descriptor_closed() {
        assert_eq!(
            go_shim_script(3),
            "IFS= read -r _go <&3 || exit 127; command -v \"$0\" >/dev/null 2>&1 || { printf 'ENOENT\\n' >&3; exit 127; }; exec \"$0\" \"$@\" 3<&-"
        );
    }

    #[test]
    fn control_fd_is_the_first_index_after_the_callers_stdio() {
        assert_eq!(control_fd(&["ignore", "pipe", "pipe"]), 3);
        assert_eq!(control_fd::<&str>(&[]), 3);
        assert_eq!(control_fd(&[1, 2, 3, 4, 5]), 5);
    }

    #[test]
    fn control_pipe_is_padded_into_place() {
        assert_eq!(
            with_control_pipe(&["a", "b"], 3, "pad", "pipe"),
            ["a", "b", "pad", "pipe"]
        );
        assert_eq!(
            with_control_pipe(&["a", "b", "c"], 3, "pad", "pipe"),
            ["a", "b", "c", "pipe"]
        );
    }

    #[test]
    fn shim_target_runs_the_command_or_the_callers_shell() {
        let args = vec!["x".to_string(), "y".to_string()];
        assert_eq!(
            shim_target("cat", &args, None),
            ShimTarget {
                program: "cat".into(),
                args: args.clone()
            }
        );
        assert_eq!(
            shim_target("cat", &args, Some(&Shell::Enabled(false))),
            ShimTarget {
                program: "cat".into(),
                args: args.clone()
            }
        );
        assert_eq!(
            shim_target("echo", &args, Some(&Shell::Enabled(true))),
            ShimTarget {
                program: "/bin/sh".into(),
                args: vec!["-c".into(), "echo x y".into()]
            }
        );
        assert_eq!(
            shim_target("echo \"$0\"", &[], Some(&Shell::Path("/bin/bash".into()))),
            ShimTarget {
                program: "/bin/bash".into(),
                args: vec!["-c".into(), "echo \"$0\"".into()]
            }
        );
    }

    #[test]
    fn enoent_matches_an_ungated_spawn() {
        let error = enoent_error("missing-program", &["x".to_string()]);
        assert_eq!(error.to_string(), "spawn missing-program ENOENT");
        assert_eq!(error.code, "ENOENT");
        assert_eq!(error.errno, -2);
        assert_eq!(error.syscall, "spawn missing-program");
        assert_eq!(error.path, "missing-program");
        assert_eq!(error.spawnargs, vec!["x".to_string()]);
    }

    /// Runs the shim script for `program` with descriptor 3 opened read-write on a file holding `control`; returns
    /// (stdout, exit code, final control file content).
    fn run_shim(control: &str, program: &str, args: &[&str]) -> (String, Option<i32>, String) {
        let dir = tempfile::tempdir().expect("tempdir");
        let ctl = dir.path().join("ctl");
        std::fs::write(&ctl, control).expect("write control");
        let output = Command::new("/bin/sh")
            .args([
                "-c",
                "script=$1; ctl=$2; shift 2; exec /bin/sh -c \"$script\" \"$@\" 3<>\"$ctl\"",
            ])
            .arg("_")
            .arg(go_shim_script(3))
            .arg(&ctl)
            .arg(program)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .expect("run shim");
        (
            String::from_utf8_lossy(&output.stdout).into_owned(),
            output.status.code(),
            std::fs::read_to_string(&ctl).expect("read control"),
        )
    }

    #[test]
    fn shim_runs_the_command_after_the_go_line() {
        let (stdout, code, _) = run_shim("go\n", "echo", &["from-the-shim"]);
        assert_eq!((stdout.as_str(), code), ("from-the-shim\n", Some(0)));
    }

    #[test]
    fn shim_exits_127_when_the_control_closes_before_go() {
        let (stdout, code, _) = run_shim("", "echo", &["never"]);
        assert_eq!((stdout.as_str(), code), ("", Some(127)));
    }

    #[test]
    fn shim_reports_a_missing_program_on_the_control_descriptor() {
        let (_, code, control) = run_shim("go\n", "maho-definitely-missing-program", &["x"]);
        assert_eq!(code, Some(127));
        assert_eq!(control, "go\nENOENT\n");
    }

    #[test]
    fn shim_runs_a_custom_shell_string_with_that_shell() {
        let target = shim_target("echo \"$0\"", &[], Some(&Shell::Path("/bin/sh".into())));
        let args: Vec<&str> = target.args.iter().map(String::as_str).collect();
        let (stdout, _, _) = run_shim("go\n", &target.program, &args);
        assert_eq!(stdout.trim(), "/bin/sh");
    }
}
