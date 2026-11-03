//! WHAT the lifecycle supervisor is launched with and WHAT it launches (senpi `host-lifecycle-launch.ts`): its hidden
//! argv route, the argv it parses, and the environment that binds the host child it spawns to this supervisor and
//! its generation. Split out of `host_lifecycle`, which keeps the supervisor's orchestration and re-exports every
//! name here.
use std::collections::HashMap;

/// Hidden internal launch route: wire-invisible, never advertised by the public CLI surface.
pub const INTERNAL_SUPERVISOR_FLAG: &str = "--internal-rpc-host-supervisor";

#[derive(Debug, PartialEq, Eq)]
pub struct SupervisorLaunch {
    pub socket: String,
    pub host_args: Vec<String>,
    pub child_command: Option<String>,
    pub child_args: Option<Vec<String>>,
    pub agent_dir: Option<String>,
    pub bind_socket: Option<String>,
    pub replace_identity: Option<crate::socket_ownership::SocketFileIdentity>,
    /// `--mcp-presenter-fd 4`: the launching CLI's enrollment channel, relayed once to the child (senpi
    /// `presenterBootstrapFd`). Only fd 4 is accepted.
    pub presenter_bootstrap_fd: Option<i32>,
    /// `--owner-fd <n>` (n >= 3): the inherited caller-lifetime pipe, distinct from the supervisor-to-host watchdog
    /// pipe (senpi `ownerFd`). A launch naming the presenter descriptor as its owner fd is refused.
    pub owner_fd: Option<i32>,
}
/// Returns the internal supervisor payload when argv selects that route: the sentinel leads argv or is preceded only
/// by complete `--extension <dir>` pairs a rebranded wrapper injects (senpi `findInternalSupervisorArgs`).
pub fn find_internal_supervisor_args(argv: &[String]) -> Option<&[String]> {
    let mut index = 0;
    while index < argv.len() {
        if argv[index] == INTERNAL_SUPERVISOR_FLAG {
            return Some(&argv[index + 1..]);
        }
        if argv[index] != "--extension" || index + 1 >= argv.len() {
            return None;
        }
        index += 2;
    }
    None
}
/// `--socket <path>` selects the public socket; every other argument is forwarded to the host CLI (senpi
/// `parseSupervisorArgs`).
pub fn parse_supervisor_args(argv: &[String]) -> Option<SupervisorLaunch> {
    let mut launch = SupervisorLaunch {
        socket: String::new(),
        host_args: vec![],
        child_command: None,
        child_args: None,
        agent_dir: None,
        bind_socket: None,
        replace_identity: None,
        presenter_bootstrap_fd: None,
        owner_fd: None,
    };
    let mut socket_seen = false;
    let mut index = 0;
    while index < argv.len() {
        let arg = &argv[index];
        if arg == crate::mcp_presenter_bootstrap::MCP_PRESENTER_FD_FLAG
            && argv.get(index + 1).map(String::as_str) == Some("4")
        {
            launch.presenter_bootstrap_fd =
                Some(crate::mcp_presenter_bootstrap::MCP_PRESENTER_BOOTSTRAP_FD);
            index += 2;
            continue;
        }
        if arg == "--owner-fd" && index + 1 < argv.len() {
            let fd = argv[index + 1].parse::<i32>().ok().filter(|fd| *fd >= 3)?;
            launch.owner_fd = Some(fd);
            index += 2;
            continue;
        }
        if index + 1 < argv.len()
            && matches!(
                arg.as_str(),
                "--socket"
                    | "--child-command"
                    | "--child-args"
                    | "--agent-dir"
                    | "--bind"
                    | "--replace"
            )
        {
            index += 1;
            let value = &argv[index];
            match arg.as_str() {
                "--socket" => {
                    launch.socket = value.clone();
                    socket_seen = true;
                }
                "--child-command" => launch.child_command = Some(value.clone()),
                "--child-args" => {
                    let parsed: serde_json::Value = serde_json::from_str(value).ok()?;
                    launch.child_args = parsed.as_array().and_then(|items| {
                        items
                            .iter()
                            .map(|item| item.as_str().map(str::to_owned))
                            .collect()
                    });
                }
                "--agent-dir" => launch.agent_dir = Some(value.clone()),
                "--bind" => launch.bind_socket = Some(value.clone()),
                "--replace" => {
                    launch.replace_identity = value
                        .split_once(':')
                        .filter(|(dev, ino)| {
                            !dev.is_empty()
                                && !ino.is_empty()
                                && dev
                                    .bytes()
                                    .chain(ino.bytes())
                                    .all(|byte| byte.is_ascii_digit())
                        })
                        .and_then(|(dev, ino)| {
                            Some(crate::socket_ownership::SocketFileIdentity {
                                dev: dev.parse().ok()?,
                                ino: ino.parse().ok()?,
                            })
                        })
                }
                _ => unreachable!(),
            }
        } else {
            launch.host_args.push(arg.clone());
        }
        index += 1;
    }
    if launch.owner_fd.is_some() && launch.owner_fd == launch.presenter_bootstrap_fd {
        return None;
    }
    socket_seen.then_some(launch)
}

/// The descriptor an ensure hands its supervisor as the caller-lifetime pipe (senpi `OWNER_WATCH_FD`).
pub const OWNER_WATCH_FD: i32 = 3;

/// The committed CLI entry this supervisor wraps; the native binary is its own entry.
pub fn resolve_cli_main_path() -> std::io::Result<std::path::PathBuf> {
    std::env::current_exe()
}
/// The host child spawn: an explicit child command is forwarded untouched, otherwise the
/// supervisor re-enters the committed CLI entry with the RPC multi-session flags.
pub fn resolve_host_child_launch(
    launch: &SupervisorLaunch,
    internal_socket: &str,
) -> std::io::Result<crate::host_launch::HostLaunch> {
    if let Some(command) = &launch.child_command {
        return Ok(crate::host_launch::HostLaunch {
            command: std::path::PathBuf::from(command),
            args: launch
                .child_args
                .clone()
                .unwrap_or_default()
                .into_iter()
                .chain(["--listen".to_owned(), format!("unix://{internal_socket}")])
                .collect(),
        });
    }
    Ok(crate::host_launch::HostLaunch {
        command: resolve_cli_main_path()?,
        args: ["--mode", "rpc", "--multi-session", "--listen"]
            .into_iter()
            .map(str::to_owned)
            .chain([format!("unix://{internal_socket}")])
            .chain(launch.host_args.iter().cloned())
            .collect(),
    })
}
/// A spawn shape that survives a Windows shell (senpi `spawnableChildLaunch`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnableLaunch {
    pub command: String,
    pub args: Vec<String>,
    pub shell: bool,
}
fn quote_windows_shell_arg(value: &str) -> String {
    let mut escaped = String::new();
    let mut backslashes = 0usize;
    for character in value.chars() {
        match character {
            '\\' => {
                backslashes += 1;
                escaped.push('\\');
            }
            '"' => {
                for _ in 0..=backslashes {
                    escaped.push('\\');
                }
                backslashes = 0;
                escaped.push('"');
            }
            '(' | ')' | '%' | '!' | '^' | '<' | '>' | '&' | '|' | ';' | ',' => {
                escaped.push('^');
                escaped.push(character);
            }
            _ => {
                backslashes = 0;
                escaped.push(character);
            }
        }
    }
    format!("\"{escaped}\"")
}
pub fn spawnable_child_launch(
    launch: &crate::host_launch::HostLaunch,
    platform: &str,
) -> SpawnableLaunch {
    let command = launch.command.to_string_lossy().into_owned();
    let extension = launch
        .command
        .extension()
        .map(|extension| extension.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if platform != "win32" || (extension != "cmd" && extension != "bat") {
        return SpawnableLaunch {
            command,
            args: launch.args.clone(),
            shell: false,
        };
    }
    SpawnableLaunch {
        command: quote_windows_shell_arg(&command),
        args: launch
            .args
            .iter()
            .map(|arg| quote_windows_shell_arg(arg))
            .collect(),
        shell: true,
    }
}

/// The stdio slot senpi gives the child's lifetime pipe (senpi `CHILD_WATCH_FD`). Rust's `Command`
/// exposes only the three standard slots, so the pipe is inherited by descriptor instead - see
/// `bind_child_watch_pipe`.
pub const HOST_CHILD_WATCH_FD: u32 = 3;
/// The pre-rename name of [`HOST_CHILD_WATCH_FD`].
pub const CHILD_WATCH_FD: u32 = HOST_CHILD_WATCH_FD;
/// Gives `child_env` the child's lifetime binding and returns the pipe ends this process keeps
/// alive for the whole supervisor run (senpi host-lifecycle.ts:513-544, stdio slot 3).
///
/// senpi duplicates the read end into stdio slot 3. Rust's `Command` exposes only the three
/// standard slots and `pre_exec` is unavailable under this crate's `unsafe_code = "deny"`, so the
/// read end's close-on-exec flag is cleared instead: the child inherits the pipe at the number this
/// process holds it at, and `HOST_WATCH_FD_ENV` carries exactly that number. The write end stays
/// here, close-on-exec, is never inherited and is never written - only its close matters.
#[cfg(unix)]
pub(crate) fn bind_child_watch_pipe(
    child_env: &mut HashMap<String, String>,
) -> std::io::Result<(std::os::fd::OwnedFd, std::os::fd::OwnedFd)> {
    use std::os::fd::AsRawFd;
    let (read_end, write_end) = rustix::pipe::pipe()?;
    rustix::io::fcntl_setfd(&write_end, rustix::io::FdFlags::CLOEXEC)
        .map_err(std::io::Error::from)?;
    rustix::io::fcntl_setfd(&read_end, rustix::io::FdFlags::empty())
        .map_err(std::io::Error::from)?;
    let descriptor = u64::try_from(read_end.as_raw_fd())
        .map_err(|_| std::io::Error::other("child watch descriptor is not representable"))?;
    child_env.insert(
        crate::host_watchdog::HOST_WATCH_FD_ENV.into(),
        descriptor.to_string(),
    );
    Ok((read_end, write_end))
}

/// What [`host_child_env`] binds the child to (senpi `spawnHostChild`'s options).
pub struct HostChildEnv<'a> {
    pub platform: &'a str,
    pub instance_id: &'a str,
    pub daemon_dir: &'a str,
    pub agent_dir: &'a str,
    pub public_socket: &'a str,
    pub internal: &'a crate::host_lifecycle::InternalSocketPath,
    pub internal_secret: bool,
    pub internal_key: bool,
    pub supervisor_start: Option<&'a crate::host_lifecycle_internal_key::SupervisorStart>,
    pub lifetime_fifo: Option<&'a std::path::Path>,
    pub cleanup_paths: &'a [std::path::PathBuf],
}

/// senpi `spawnHostChild`'s environment: the generation identity, the supervisor's pid and start for the lifetime
/// poll, the lifetime FIFO, the private scratch directory, the win32-only secret file, the stdin key marker, the
/// crash-path cleanup list and (POSIX) the public socket the child owns ownership-checked.
pub fn host_child_env(
    parent: HashMap<String, String>,
    options: &HostChildEnv<'_>,
) -> HashMap<String, String> {
    use crate::host_watchdog::{
        HOST_CLEANUP_PATHS_ENV, HOST_PUBLIC_SOCKET_ENV, HOST_SCRATCH_DIR_ENV,
        HOST_WATCH_LIFETIME_FIFO_ENV, HOST_WATCH_PPID_ENV, HOST_WATCH_PSTART_ENV,
        HOST_WATCH_PSTART_TICKS_ENV,
    };
    let mut env = crate::host_successor::successor_env(
        parent,
        0,
        options.instance_id,
        options.daemon_dir,
        Some(options.agent_dir),
        &HashMap::new(),
    );
    // win32 names no public socket to the child: a named pipe has no filesystem entry to own.
    if options.platform != "win32" {
        env.insert(HOST_PUBLIC_SOCKET_ENV.into(), options.public_socket.into());
    }
    // The secret file is win32-only: the sealed hop replaces it on POSIX.
    if options.platform == "win32"
        && options.internal_secret
        && let Some(secret_path) = &options.internal.secret_path
    {
        env.insert(
            crate::socket_transport::SOCKET_SECRET_FILE_ENV.into(),
            secret_path.to_string_lossy().into_owned(),
        );
    }
    if let Some(dir) = &options.internal.dir {
        env.insert(
            HOST_SCRATCH_DIR_ENV.into(),
            dir.to_string_lossy().into_owned(),
        );
    }
    // The ppid fallback covers a lifetime descriptor that could not be inherited.
    env.insert(HOST_WATCH_PPID_ENV.into(), std::process::id().to_string());
    env.insert(
        HOST_CLEANUP_PATHS_ENV.into(),
        options
            .cleanup_paths
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("\n"),
    );
    // The child's lifetime poll compares the supervisor's start on one clock per platform.
    if let Some(start) = options.supervisor_start {
        if let Some(time) = &start.time {
            env.insert(HOST_WATCH_PSTART_ENV.into(), time.clone());
        }
        if let Some(ticks) = &start.ticks {
            env.insert(HOST_WATCH_PSTART_TICKS_ENV.into(), ticks.clone());
        }
    }
    if options.internal_key {
        env.insert(
            crate::internal_bootstrap::INTERNAL_KEY_STDIN_ENV.into(),
            "1".into(),
        );
    }
    if let Some(fifo) = options.lifetime_fifo {
        env.insert(
            HOST_WATCH_LIFETIME_FIFO_ENV.into(),
            fifo.to_string_lossy().into_owned(),
        );
    }
    env
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn supervisor_route_skips_only_complete_injected_prefix() {
        let args = [
            "--extension",
            "dir",
            INTERNAL_SUPERVISOR_FLAG,
            "--socket",
            "sock",
        ]
        .map(str::to_owned);
        assert_eq!(find_internal_supervisor_args(&args), Some(&args[3..]));
        assert!(
            find_internal_supervisor_args(&["--model".into(), INTERNAL_SUPERVISOR_FLAG.into()])
                .is_none()
        );
        assert!(
            find_internal_supervisor_args(&["--extension".into(), INTERNAL_SUPERVISOR_FLAG.into()])
                .is_none()
        );
    }
    #[test]
    fn supervisor_parser_keeps_forwarded_args_and_replacement_identity() {
        let args = [
            "--socket",
            "sock",
            "--replace",
            "1:2",
            "--child-args",
            "[\"one\"]",
            "--mode",
            "rpc",
        ]
        .map(str::to_owned);
        let launch = parse_supervisor_args(&args).unwrap();
        assert_eq!(launch.child_args, Some(vec!["one".into()]));
        assert_eq!(launch.host_args, vec!["--mode", "rpc"]);
        assert_eq!(
            launch.replace_identity,
            Some(crate::socket_ownership::SocketFileIdentity { dev: 1, ino: 2 })
        );
        assert!(
            parse_supervisor_args(&[
                "--socket".into(),
                "sock".into(),
                "--child-args".into(),
                "{".into()
            ])
            .is_none()
        );
    }
    #[test]
    fn supervisor_parser_takes_only_the_reserved_presenter_descriptor() {
        let launch = parse_supervisor_args(
            &[
                "--socket",
                "sock",
                "--mcp-presenter-fd",
                "4",
                "--mode",
                "rpc",
            ]
            .map(str::to_owned),
        )
        .unwrap();
        assert_eq!(launch.presenter_bootstrap_fd, Some(4));
        assert_eq!(launch.host_args, vec!["--mode", "rpc"]);
        let other = parse_supervisor_args(
            &["--socket", "sock", "--mcp-presenter-fd", "5"].map(str::to_owned),
        )
        .unwrap();
        assert_eq!(other.presenter_bootstrap_fd, None);
        assert_eq!(other.host_args, vec!["--mcp-presenter-fd", "5"]);
    }
    #[test]
    fn supervisor_route_refuses_a_positional_dash_dash_or_dangling_prefix() {
        let route = |argv: &[&str]| {
            let argv: Vec<String> = argv.iter().map(|arg| (*arg).to_owned()).collect();
            find_internal_supervisor_args(&argv).map(<[String]>::to_vec)
        };
        assert_eq!(
            route(&[INTERNAL_SUPERVISOR_FLAG, "--socket", "/tmp/qa.sock"]),
            Some(vec!["--socket".to_owned(), "/tmp/qa.sock".to_owned()])
        );
        assert!(
            route(&[
                "explain this",
                INTERNAL_SUPERVISOR_FLAG,
                "--socket",
                "/tmp/x"
            ])
            .is_none()
        );
        assert!(route(&["--", INTERNAL_SUPERVISOR_FLAG, "--socket", "/tmp/x"]).is_none());
        assert!(route(&["--print", INTERNAL_SUPERVISOR_FLAG]).is_none());
        assert!(route(&["--extension"]).is_none());
        assert!(route(&["--mode", "rpc"]).is_none());
    }
    #[test]
    fn the_owner_fd_is_validated_and_may_not_be_the_presenter_descriptor() {
        let parse = |argv: &[&str]| {
            parse_supervisor_args(&argv.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>())
        };
        assert_eq!(
            parse(&["--socket", "s", "--owner-fd", "3"])
                .unwrap()
                .owner_fd,
            Some(OWNER_WATCH_FD)
        );
        assert!(parse(&["--socket", "s", "--owner-fd", "2"]).is_none());
        assert!(parse(&["--socket", "s", "--owner-fd", "x"]).is_none());
        assert!(
            parse(&[
                "--socket",
                "s",
                "--owner-fd",
                "4",
                "--mcp-presenter-fd",
                "4"
            ])
            .is_none()
        );
        assert!(parse(&["--owner-fd", "3"]).is_none());
    }
    #[test]
    fn explicit_child_commands_are_forwarded_and_the_default_reenters_this_executable() {
        let mut launch = parse_supervisor_args(
            &["--socket", "/tmp/qa-public.sock", "--provider", "mock"].map(str::to_owned),
        )
        .unwrap();
        let default = resolve_host_child_launch(&launch, "/tmp/internal.sock").unwrap();
        assert_eq!(default.command, std::env::current_exe().unwrap());
        assert_eq!(
            default.args,
            [
                "--mode",
                "rpc",
                "--multi-session",
                "--listen",
                "unix:///tmp/internal.sock",
                "--provider",
                "mock"
            ]
        );
        launch.child_command = Some("/opt/branded/omo".into());
        launch.child_args = Some(vec!["--mode".into(), "rpc".into()]);
        let explicit = resolve_host_child_launch(&launch, "/tmp/internal.sock").unwrap();
        assert_eq!(
            explicit.command,
            std::path::PathBuf::from("/opt/branded/omo")
        );
        assert_eq!(
            explicit.args,
            ["--mode", "rpc", "--listen", "unix:///tmp/internal.sock"]
        );
    }
    #[test]
    fn cmd_launchers_are_quoted_before_shell_metacharacter_escaping() {
        let launch = spawnable_child_launch(
            &crate::host_launch::HostLaunch {
                command: "C:\\Program Files\\launcher.cmd".into(),
                args: vec!["hello world".into(), "x&y".into()],
            },
            "win32",
        );
        assert!(launch.shell);
        assert_eq!(launch.command, "\"C:\\Program Files\\launcher.cmd\"");
        assert_eq!(launch.args, ["\"hello world\"", "\"x^&y\""]);
        let posix = spawnable_child_launch(
            &crate::host_launch::HostLaunch {
                command: "/bin/launcher.cmd".into(),
                args: vec!["x&y".into()],
            },
            "linux",
        );
        assert!(!posix.shell);
        assert_eq!(posix.args, ["x&y"]);
    }
    #[test]
    fn the_child_env_binds_the_child_to_this_supervisor_and_generation() {
        let internal = crate::host_lifecycle::InternalSocketPath {
            socket: "/tmp/hop/host.sock".into(),
            dir: Some("/tmp/hop".into()),
            secret_path: Some("/tmp/hop/.secret".into()),
        };
        let start = crate::host_lifecycle_internal_key::SupervisorStart {
            time: Some("123".into()),
            ticks: Some("456".into()),
        };
        let cleanup = ["/d/pid".into(), "/d/host.pid".into()];
        let options = HostChildEnv {
            platform: "linux",
            instance_id: "gen-1",
            daemon_dir: "/d",
            agent_dir: "/agent",
            public_socket: "/tmp/pub.sock",
            internal: &internal,
            internal_secret: false,
            internal_key: true,
            supervisor_start: Some(&start),
            lifetime_fifo: Some(std::path::Path::new("/tmp/hop/lifetime.fifo")),
            cleanup_paths: &cleanup,
        };
        let env = host_child_env(HashMap::new(), &options);
        assert_eq!(env[crate::protocol_identity::HOST_INSTANCE_ID_ENV], "gen-1");
        assert_eq!(env[crate::host_daemon_paths::HOST_DAEMON_DIR_ENV], "/d");
        assert_eq!(
            env[crate::host_watchdog::HOST_PUBLIC_SOCKET_ENV],
            "/tmp/pub.sock"
        );
        assert_eq!(env[crate::host_watchdog::HOST_SCRATCH_DIR_ENV], "/tmp/hop");
        assert_eq!(
            env[crate::host_watchdog::HOST_WATCH_PPID_ENV],
            std::process::id().to_string()
        );
        assert_eq!(env[crate::host_watchdog::HOST_WATCH_PSTART_ENV], "123");
        assert_eq!(
            env[crate::host_watchdog::HOST_WATCH_PSTART_TICKS_ENV],
            "456"
        );
        assert_eq!(
            env[crate::host_watchdog::HOST_WATCH_LIFETIME_FIFO_ENV],
            "/tmp/hop/lifetime.fifo"
        );
        assert_eq!(env[crate::internal_bootstrap::INTERNAL_KEY_STDIN_ENV], "1");
        assert_eq!(
            env[crate::host_watchdog::HOST_CLEANUP_PATHS_ENV],
            "/d/pid\n/d/host.pid"
        );
        assert!(!env.contains_key(crate::socket_transport::SOCKET_SECRET_FILE_ENV));
        let win32 = host_child_env(
            HashMap::new(),
            &HostChildEnv {
                platform: "win32",
                internal_secret: true,
                internal_key: false,
                supervisor_start: None,
                lifetime_fifo: None,
                ..options
            },
        );
        assert!(!win32.contains_key(crate::host_watchdog::HOST_PUBLIC_SOCKET_ENV));
        assert!(!win32.contains_key(crate::internal_bootstrap::INTERNAL_KEY_STDIN_ENV));
        assert_eq!(
            win32[crate::socket_transport::SOCKET_SECRET_FILE_ENV],
            "/tmp/hop/.secret"
        );
    }
}
