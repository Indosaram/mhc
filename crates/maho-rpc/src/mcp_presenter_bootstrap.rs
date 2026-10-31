//! The reserved presenter bootstrap descriptor and its bounded relay (senpi `mcp-presenter-bootstrap.ts`).
//!
//! The descriptor is not authority: only the live channel's enrollment transcript can issue grants. A launching
//! CLI hands its enrollment channel to the host it starts at fd 4; a supervisor relays it once to its own child.
use crate::mcp_presenter_wire::BoxedPrivateStream;
use serde_json::Value;

pub const MCP_PRESENTER_BOOTSTRAP_FD: i32 = 4;
pub const MCP_PRESENTER_FD_FLAG: &str = "--mcp-presenter-fd";
pub const MCP_PRESENTER_ENFORCEMENT_FLAG: &str = "--mcp-presenter-enforcement";
const RELAY_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

/// The launch this CLI is performing. A v2 enrollment for a different kind, or naming a predecessor other than the
/// host this launch replaces (`None` inside `Some` for an initial launch), is dropped before the host sees it.
/// `predecessor_host_instance: None` at the outer level means nothing proves the replaced host, so no replacement
/// enrolls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresenterLaunch {
    /// `initial` or `replacement`.
    pub kind: &'static str,
    pub predecessor_host_instance: Option<Option<String>>,
}

/// Takes ownership of the inherited reserved descriptor when it is a socket or FIFO (senpi
/// `presenterBootstrapStream`'s `fstatSync` gate), and marks it close-on-exec so no process this host spawns
/// inherits the enrollment channel. Dropping the result closes fd 4. Anything else at fd 4 is left untouched and
/// yields `None`: bootstrap is optional authority, never a reason to terminate the shared host.
///
/// Adoption happens at most once per process; a second call yields `None` instead of a second owner.
#[cfg(unix)]
#[allow(unsafe_code)]
pub fn adopt_presenter_bootstrap_fd(fd: i32) -> Option<std::os::fd::OwnedFd> {
    use std::os::fd::{BorrowedFd, FromRawFd as _, OwnedFd};
    use std::os::unix::fs::FileTypeExt as _;
    static ADOPTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if fd != MCP_PRESENTER_BOOTSTRAP_FD {
        return None;
    }
    // SAFETY: `fcntl(F_GETFD)` takes no pointers and only reports whether `fd` is open (`-1`/EBADF otherwise).
    if unsafe { libc::fcntl(fd, libc::F_GETFD) } == -1 {
        return None;
    }
    // SAFETY: fd 4 was just observed open, and the launch contract (`--mcp-presenter-fd 4`) reserves it for the
    // bootstrap channel, so nothing else in this process closes it while this borrow lives.
    let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
    let kind = std::fs::File::from(borrowed.try_clone_to_owned().ok()?)
        .metadata()
        .ok()?
        .file_type();
    if !kind.is_socket() && !kind.is_fifo() {
        return None;
    }
    if ADOPTED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return None;
    }
    // SAFETY: fd 4 is open and is a socket or FIFO this process inherited for the bootstrap; the one-shot `ADOPTED`
    // latch makes this the descriptor's only owner, so it is closed exactly once, when the `OwnedFd` drops.
    let owned = unsafe { OwnedFd::from_raw_fd(fd) };
    rustix::io::fcntl_setfd(&owned, rustix::io::FdFlags::CLOEXEC).ok()?;
    Some(owned)
}

/// An adopted bootstrap descriptor as an owned stream, or `None` when it is not a socket or FIFO (senpi
/// `presenterBootstrapStream`). Needs a tokio runtime.
#[cfg(unix)]
pub fn presenter_bootstrap_stream_from(owned: std::os::fd::OwnedFd) -> Option<BoxedPrivateStream> {
    use std::os::fd::OwnedFd;
    use std::os::unix::fs::FileTypeExt as _;
    let file = std::fs::File::from(owned);
    let kind = file.metadata().ok()?.file_type();
    let owned = OwnedFd::from(file);
    match () {
        () if kind.is_socket() => {
            let stream = std::os::unix::net::UnixStream::from(owned);
            stream.set_nonblocking(true).ok()?;
            tokio::net::UnixStream::from_std(stream)
                .ok()
                .map(|stream| Box::new(stream) as BoxedPrivateStream)
        }
        () if kind.is_fifo() => {
            let read =
                tokio::net::unix::pipe::Receiver::from_owned_fd(owned.try_clone().ok()?).ok()?;
            let write = tokio::net::unix::pipe::Sender::from_owned_fd(owned).ok()?;
            Some(Box::new(tokio::io::join(read, write)) as BoxedPrivateStream)
        }
        _ => None,
    }
}

/// The inherited descriptor as an owned stream (senpi `presenterBootstrapStream`): adopts fd 4 (closed with the
/// stream, close-on-exec meanwhile) and wraps it. Needs a tokio runtime.
#[cfg(unix)]
pub fn presenter_bootstrap_stream(fd: i32) -> Option<BoxedPrivateStream> {
    presenter_bootstrap_stream_from(adopt_presenter_bootstrap_fd(fd)?)
}
#[cfg(not(unix))]
pub fn presenter_bootstrap_stream(_fd: i32) -> Option<BoxedPrivateStream> {
    None
}
/// The adopted bootstrap descriptor; no platform without unix descriptors adopts one.
#[cfg(unix)]
pub type AdoptedPresenterBootstrap = std::os::fd::OwnedFd;
#[cfg(not(unix))]
pub type AdoptedPresenterBootstrap = std::convert::Infallible;
#[cfg(not(unix))]
pub fn adopt_presenter_bootstrap_fd(_fd: i32) -> Option<AdoptedPresenterBootstrap> {
    None
}

/// The child's end of a bootstrap channel and the argv/command rewrite that lands it at fd 4.
#[cfg(unix)]
pub struct ChildBootstrap {
    /// This process's end; the relay reads and writes it.
    pub parent: tokio::net::UnixStream,
    /// Kept open until the child is spawned, then dropped so only the child holds it.
    pub child: std::os::fd::OwnedFd,
}
#[cfg(unix)]
impl ChildBootstrap {
    pub fn new() -> std::io::Result<Self> {
        let (parent, child) = std::os::unix::net::UnixStream::pair()?;
        parent.set_nonblocking(true)?;
        let child = std::os::fd::OwnedFd::from(child);
        // Inherited by the shell wrapper below, which moves it to fd 4 and closes the original number.
        rustix::io::fcntl_setfd(&child, rustix::io::FdFlags::empty())
            .map_err(std::io::Error::from)?;
        Ok(Self {
            parent: tokio::net::UnixStream::from_std(parent)?,
            child,
        })
    }
    /// SAFETY-DIVERGENCE(senpi spawn stdio slot 4): Rust's `Command` exposes only the three standard slots and this
    /// crate denies the `unsafe` a `pre_exec` `dup2` needs, so `/bin/sh` performs it: `exec "$0" "$@" 4<&N N<&-`
    /// replaces the shell with the target (same pid) holding the channel at fd 4 and nothing else.
    pub fn command(&self, program: &std::ffi::OsStr) -> tokio::process::Command {
        use std::os::fd::AsRawFd;
        let fd = self.child.as_raw_fd();
        let mut command = tokio::process::Command::new("/bin/sh");
        command
            .arg("-c")
            .arg(format!("exec \"$0\" \"$@\" 4<&{fd} {fd}<&-"))
            .arg(program);
        command
    }
}

/// One ensure/handoff request's presenter options (senpi `EnsureHostOptions` / `HandoffHostOptions`
/// `presenterBootstrap` + `mcpPresenterEnforcement`): the enrollment channel is consumed by the one spawn it
/// enrolls, and enforcement is the deployment opt-in the started host keeps.
#[derive(Default)]
pub struct PresenterLaunchOptions {
    pub bootstrap: std::sync::Mutex<Option<BoxedPrivateStream>>,
    /// Deployment opt-in, never a project setting or proof of enrollment. Defaults off for a new host.
    pub enforcement: bool,
}
tokio::task_local! {static PRESENTER_LAUNCH: std::sync::Arc<PresenterLaunchOptions>;}
/// Runs one host request with its presenter options in scope (the CLI's `host ensure|handoff --mcp-presenter-fd 4
/// --mcp-presenter-enforcement`).
pub async fn with_presenter_launch<F: std::future::Future>(
    options: PresenterLaunchOptions,
    run: F,
) -> F::Output {
    PRESENTER_LAUNCH
        .scope(std::sync::Arc::new(options), run)
        .await
}
/// Whether the request in scope asked for enforcement.
pub fn presenter_enforcement() -> bool {
    PRESENTER_LAUNCH
        .try_with(|options| options.enforcement)
        .unwrap_or(false)
}
/// Takes the request's enrollment channel; only the first spawn gets it (it never enrolls on reuse).
pub fn take_presenter_bootstrap() -> Option<BoxedPrivateStream> {
    PRESENTER_LAUNCH
        .try_with(|options| crate::mcp_form_dialog::lock(&options.bootstrap).take())
        .ok()
        .flatten()
}
/// The supervisor argv flags a launch carries: `--mcp-presenter-fd 4` when a channel is relayed, and
/// `--mcp-presenter-enforcement` when the deployment enforces (senpi `host-ensure-start` / `host-successor`).
pub fn presenter_launch_args(bootstrap: bool, enforcement: bool) -> Vec<String> {
    let mut args = Vec::new();
    if bootstrap {
        args.extend([MCP_PRESENTER_FD_FLAG.to_owned(), "4".to_owned()]);
    }
    if enforcement {
        args.push(MCP_PRESENTER_ENFORCEMENT_FLAG.to_owned());
    }
    args
}

/// Spawns `command` (already carrying its args, env and stdio) with `bootstrap` relayed at the child's fd 4, and
/// returns the child plus the relay that ends at the enrollment acknowledgement (senpi `relayPresenterBootstrap`).
#[cfg(unix)]
pub fn spawn_with_presenter_bootstrap(
    program: &std::ffi::OsStr,
    args: &[String],
    configure: impl FnOnce(&mut tokio::process::Command),
    bootstrap: Option<BoxedPrivateStream>,
    launch: PresenterLaunch,
) -> std::io::Result<(tokio::process::Child, Option<tokio::task::JoinHandle<()>>)> {
    let Some(upstream) = bootstrap else {
        let mut command = tokio::process::Command::new(program);
        command.args(args);
        configure(&mut command);
        return Ok((command.spawn()?, None));
    };
    let channel = ChildBootstrap::new()?;
    let mut command = channel.command(program);
    command.args(args);
    configure(&mut command);
    let child = command.spawn()?;
    let ChildBootstrap {
        parent,
        child: inherited,
    } = channel;
    drop(inherited);
    let relay = tokio::spawn(async move {
        relay_presenter_bootstrap(upstream, parent, Some(launch), std::future::pending()).await;
    });
    Ok((child, Some(relay)))
}

fn line_of(value: &Value) -> String {
    format!("{value}\n")
}

/// Relay one bounded enrollment exchange between `upstream` (the CLI's channel) and the child's fd 4 (senpi
/// `relayPresenterBootstrap`). It ends at the first `enrolled`/`unavailable` acknowledgement, on either side's
/// EOF or error, at the 30 s deadline, or when the child exits (`exited` resolves).
pub async fn relay_presenter_bootstrap<U, D>(
    upstream: U,
    downstream: D,
    launch: Option<PresenterLaunch>,
    exited: impl std::future::Future<Output = ()>,
) where
    U: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    D: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (mut up_read, mut up_write) = tokio::io::split(upstream);
    let (mut down_read, mut down_write) = tokio::io::split(downstream);
    let limit = crate::mcp_presenter_wire::PRIVATE_FRAME_LIMIT;
    let (Ok(mut up_lines), Ok(mut down_lines)) = (
        crate::jsonl::JsonlLineReader::new(limit),
        crate::jsonl::JsonlLineReader::new(limit),
    ) else {
        return;
    };
    let deadline = tokio::time::sleep(RELAY_DEADLINE);
    tokio::pin!(deadline);
    tokio::pin!(exited);
    let mut up_chunk = [0u8; 4096];
    let mut down_chunk = [0u8; 4096];
    loop {
        tokio::select! {
            () = &mut deadline => return,
            () = &mut exited => return,
            read = up_read.read(&mut up_chunk) => {
                let Ok(count) = read else { return };
                if count == 0 { return; }
                for record in up_lines.push(&up_chunk[..count]) {
                    let crate::jsonl::LineRecord::Line(line) = record else { return };
                    if let Some(launch) = &launch {
                        let Ok(value) = serde_json::from_str::<Value>(&line) else { return };
                        if value["type"] == "enroll" && value["version"] == 2 {
                            let kind_matches = value.get("launchKind").and_then(Value::as_str) == Some(launch.kind);
                            let predecessor = value.get("predecessorHostInstance");
                            let predecessor_matches = match (&launch.predecessor_host_instance, predecessor) {
                                (None, _) | (_, None) => false,
                                (Some(None), Some(Value::Null)) => true,
                                (Some(Some(expected)), Some(Value::String(actual))) => expected == actual,
                                _ => false,
                            };
                            if !kind_matches || !predecessor_matches {
                                return;
                            }
                        }
                    }
                    if down_write.write_all(format!("{line}\n").as_bytes()).await.is_err() {
                        return;
                    }
                }
            }
            read = down_read.read(&mut down_chunk) => {
                let Ok(count) = read else { return };
                if count == 0 { return; }
                for record in down_lines.push(&down_chunk[..count]) {
                    let crate::jsonl::LineRecord::Line(line) = record else { return };
                    let Ok(value) = serde_json::from_str::<Value>(&line) else { return };
                    let acknowledging = matches!(value["type"].as_str(), Some("enrolled" | "unavailable"));
                    if up_write.write_all(line_of(&value).as_bytes()).await.is_err()
                        || up_write.flush().await.is_err()
                    {
                        return;
                    }
                    if acknowledging {
                        return;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

    async fn relay(
        launch: Option<PresenterLaunch>,
        enroll: Value,
    ) -> (Option<String>, Option<String>) {
        let (mut cli, upstream) = tokio::io::duplex(4096);
        let (downstream, host) = tokio::io::duplex(4096);
        let (host_read, mut host_write) = tokio::io::split(host);
        let relay = tokio::spawn(async move {
            relay_presenter_bootstrap(upstream, downstream, launch, std::future::pending()).await;
        });
        cli.write_all(line_of(&enroll).as_bytes()).await.unwrap();
        let mut host_lines = tokio::io::BufReader::new(host_read).lines();
        let seen = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            host_lines.next_line(),
        )
        .await
        .ok()
        .and_then(Result::ok)
        .flatten();
        if seen.is_some() {
            host_write
                .write_all(b"{\"type\":\"enrolled\",\"version\":2}\n")
                .await
                .unwrap();
        }
        relay.await.unwrap();
        let (cli_read, _) = tokio::io::split(cli);
        let answer = tokio::io::BufReader::new(cli_read)
            .lines()
            .next_line()
            .await
            .ok()
            .flatten();
        (seen, answer)
    }

    #[tokio::test]
    async fn relays_a_matching_enrollment_and_ends_at_the_acknowledgement() {
        let launch = PresenterLaunch {
            kind: "initial",
            predecessor_host_instance: Some(None),
        };
        let (seen, answer) = relay(
            Some(launch),
            json!({"type":"enroll","version":2,"launchKind":"initial","predecessorHostInstance":null}),
        )
        .await;
        assert!(seen.is_some());
        assert_eq!(
            answer.as_deref(),
            Some("{\"type\":\"enrolled\",\"version\":2}")
        );
    }

    #[tokio::test]
    async fn drops_an_enrollment_for_another_launch_before_the_host_sees_it() {
        for (launch, enroll) in [
            (
                PresenterLaunch {
                    kind: "initial",
                    predecessor_host_instance: Some(None),
                },
                json!({"type":"enroll","version":2,"launchKind":"replacement","predecessorHostInstance":"x"}),
            ),
            (
                PresenterLaunch {
                    kind: "replacement",
                    predecessor_host_instance: Some(Some("old".into())),
                },
                json!({"type":"enroll","version":2,"launchKind":"replacement","predecessorHostInstance":"other"}),
            ),
            (
                PresenterLaunch {
                    kind: "replacement",
                    predecessor_host_instance: None,
                },
                json!({"type":"enroll","version":2,"launchKind":"replacement","predecessorHostInstance":"old"}),
            ),
        ] {
            let (seen, answer) = relay(Some(launch), enroll).await;
            assert_eq!(seen, None);
            assert_eq!(answer, None);
        }
    }

    #[test]
    fn only_the_reserved_descriptor_is_adopted() {
        assert!(adopt_presenter_bootstrap_fd(3).is_none());
    }

    const ADOPTION_CHILD_ENV: &str = "MAHO_RPC_PRESENTER_FD_ADOPTION_CHILD";

    /// Re-executes this test binary with a socket at fd 4. The child adopts it, then spawns a process that must
    /// not see fd 4, and checks fd 4 is gone once dropped; the parent checks the child test ran and passed.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn adopted_bootstrap_fd_is_closed_on_drop_and_never_inherited() {
        if std::env::var_os(ADOPTION_CHILD_ENV).is_some() {
            let owned =
                adopt_presenter_bootstrap_fd(MCP_PRESENTER_BOOTSTRAP_FD).expect("fd 4 adopted");
            assert!(adopt_presenter_bootstrap_fd(MCP_PRESENTER_BOOTSTRAP_FD).is_none());
            assert!(
                rustix::io::fcntl_getfd(&owned)
                    .unwrap()
                    .contains(rustix::io::FdFlags::CLOEXEC)
            );
            let status = std::process::Command::new("/bin/sh")
                .args(["-c", "test ! -e /proc/self/fd/4"])
                .status()
                .unwrap();
            assert!(status.success(), "a spawned child inherited fd 4");
            drop(owned);
            assert!(!std::path::Path::new("/proc/self/fd/4").exists());
            return;
        }
        let channel = ChildBootstrap::new().unwrap();
        let exe = std::env::current_exe().unwrap();
        let mut command = channel.command(exe.as_os_str());
        command
            .args([
                "--exact",
                "mcp_presenter_bootstrap::tests::adopted_bootstrap_fd_is_closed_on_drop_and_never_inherited",
                "--test-threads=1",
            ])
            .env(ADOPTION_CHILD_ENV, "1")
            .stdout(std::process::Stdio::piped());
        let child = command.spawn().unwrap();
        drop(channel);
        let output =
            tokio::time::timeout(std::time::Duration::from_secs(30), child.wait_with_output())
                .await
                .unwrap()
                .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "{stdout}");
        assert!(stdout.contains("1 passed"), "{stdout}");
    }
}
