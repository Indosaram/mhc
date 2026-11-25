//! The supervisor's part in a coordinated startup (senpi#4602, A-amended sections 1-3): it reports
//! its own milestones on the startup pipe, relays its host child's, records the attempt nonce in its
//! own generation directory for recovery, and - because the launcher's controller now decides when a
//! start has failed - retries a silent internal-readiness probe or a failed first observer connection
//! inside the attempt's budget instead of ending the generation on the first 10 s / 5 s miss.

use crate::host_daemon_state::write_state_file;
use crate::host_endpoint_owner::{
    AdoptEndpointOptions, adopt_endpoint, claim_endpoint, endpoint_owner_token,
};
use crate::host_lifecycle_launch::SupervisorLaunch;
use crate::host_startup_beacon::{
    relay_host_startup_events, report_startup_milestone, startup_hand_off,
};
use crate::host_startup_wire::{
    ADOPTION_GRANT_FILE, MAX_STARTUP_EVENT_BYTES, STARTUP_ATTEMPT_FILE, STARTUP_EVENT_FD,
    StartupBindingKind, StartupMilestone, startup_grant_line,
};
use crate::host_supervisor_log::supervisor_log;
use crate::socket_ownership::{SocketFileIdentity, stat_socket_identity, unlink_owned_socket};
use futures::future::BoxFuture;
use serde_json::{Value, json};
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};

/// How long the supervisor waits for an uncoordinated host child to start listening; it exits when the child misses this.
pub const HOST_CHILD_LISTEN_MS: u64 = 30_000;
const OBSERVER_RETRY_DELAY_MS: u64 = 250;

/// Declared before the host child is spawned: a custom child command cannot report its own progress.
pub fn declare_host_child(child_command: Option<&str>) {
    if child_command.is_some_and(|cmd| !cmd.is_empty()) {
        report_startup_milestone(StartupMilestone::HostOpaque);
    }
}

/// Whether the host child gets the startup pipe: only a coordinated launch of the engine's own host.
pub fn host_child_reports_startup(child_command: Option<&str>) -> bool {
    startup_hand_off().is_some() && child_command.is_none_or(str::is_empty)
}

pub async fn host_child_spawned<R>(
    child_startup_stream: Option<R>,
    generation_dir: &Path,
    supervisor_start_time: Option<&str>,
) -> io::Result<()>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    let Some(hand_off) = startup_hand_off() else {
        return Ok(());
    };
    relay_host_startup_events(child_startup_stream);
    let path = generation_dir.join(STARTUP_ATTEMPT_FILE);
    let payload = json!({
        "attempt": hand_off.binding.attempt,
        "pid": std::process::id(),
        "processStartTime": supervisor_start_time,
    });
    write_state_file(&path, &payload).map_err(|err| err.source)?;
    report_startup_milestone(StartupMilestone::SupervisorSetup);
    Ok(())
}

pub async fn await_internal_startup<W, WFut, O, OFut>(
    wait_for_listener: W,
    mut open_observer: O,
) -> Result<(), String>
where
    W: FnOnce(u64, bool) -> WFut,
    WFut: Future<Output = Result<(), String>>,
    O: FnMut() -> OFut,
    OFut: Future<Output = Result<(), String>>,
{
    let hand_off = startup_hand_off();
    let deadline = tokio::time::Instant::now()
        + Duration::from_millis(hand_off.as_ref().map_or(0, |h| h.budget_ms));
    let budget_ms = hand_off
        .as_ref()
        .map_or(HOST_CHILD_LISTEN_MS, |h| h.budget_ms);
    wait_for_listener(budget_ms, hand_off.is_some()).await?;
    report_startup_milestone(StartupMilestone::InternalReady);
    loop {
        match open_observer().await {
            Ok(()) => break,
            Err(reason) => {
                let unproven = reason == "host_identity_unproven" || reason == "auth_failed";
                if hand_off.is_none()
                    || unproven
                    || tokio::time::Instant::now() + Duration::from_millis(OBSERVER_RETRY_DELAY_MS)
                        >= deadline
                {
                    return Err(reason);
                }
                tokio::time::sleep(Duration::from_millis(OBSERVER_RETRY_DELAY_MS)).await;
            }
        }
    }
    report_startup_milestone(StartupMilestone::ObserverReady);
    Ok(())
}

pub fn supervisor_published() {
    report_startup_milestone(StartupMilestone::PublicPublished);
}

#[cfg(unix)]
pub struct PublishSupervisorEndpointOptions<'a, B>
where
    B: FnOnce(String) -> BoxFuture<'static, io::Result<tokio::net::UnixListener>> + Send + 'static,
{
    pub launch: &'a SupervisorLaunch,
    pub replace_owner: Option<String>,
    pub public_endpoint: Option<Arc<crate::rpc_public_key::RpcPublicEndpoint>>,
    pub generation_dir: &'a Path,
    pub instance_id: &'a str,
    pub generation: u64,
    pub platform: &'a str,
    pub public_socket_owned: &'a mut bool,
    pub prepare_and_listen: B,
}

#[cfg(unix)]
pub async fn publish_supervisor_endpoint<B>(
    options: PublishSupervisorEndpointOptions<'_, B>,
) -> io::Result<tokio::net::UnixListener>
where
    B: FnOnce(String) -> BoxFuture<'static, io::Result<tokio::net::UnixListener>> + Send + 'static,
{
    let token = endpoint_owner_token(
        &options.launch.socket,
        options.instance_id,
        options.generation,
    );
    let Some(bind_socket) = options.launch.bind_socket.clone() else {
        let socket = options.launch.socket.clone();
        let socket_owned = socket.clone();
        let prepare_and_listen = options.prepare_and_listen;
        let listener = std::sync::Arc::new(std::sync::Mutex::new(None));
        let holder = listener.clone();
        claim_endpoint(
            &socket,
            options.platform,
            token,
            Some(Box::pin(async move {
                let bound = prepare_and_listen(socket_owned).await?;
                *holder
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(bound);
                Ok(())
            })),
        )
        .await?;
        if let Some(ep) = &options.public_endpoint {
            ep.publish().map_err(io::Error::other)?;
        }
        *options.public_socket_owned = true;
        let listener = listener
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .ok_or_else(|| io::Error::other("listener not bound"))?;
        return Ok(listener);
    };

    let listener = (options.prepare_and_listen)(bind_socket.clone()).await?;
    let bound: Option<SocketFileIdentity> = stat_socket_identity(Path::new(&bind_socket))?;
    let generation_dir = options.generation_dir.to_path_buf();
    let result: io::Result<()> = async {
        let granted = await_publication_grant(&generation_dir).await?;
        if let Some(ep) = &options.public_endpoint {
            ep.publish().map_err(io::Error::other)?;
        }
        *options.public_socket_owned = true;
        adopt_endpoint(AdoptEndpointOptions {
            bind_socket: bind_socket.clone(),
            public_socket: options.launch.socket.clone(),
            platform: options.platform.to_owned(),
            expected: options.launch.replace_identity,
            expected_owner: options.replace_owner,
            token,
            granted,
            hooks: None,
        })
        .await
    }
    .await;

    if let Err(err) = result {
        unlink_owned_socket(&bind_socket, bound, options.platform, |msg| {
            supervisor_log(&msg);
        });
        return Err(err);
    }
    Ok(listener)
}

type GrantCheck = Box<dyn FnOnce() -> BoxFuture<'static, bool> + Send>;

async fn await_publication_grant(generation_dir: &Path) -> io::Result<Option<GrantCheck>> {
    let Some(hand_off) = startup_hand_off() else {
        return Ok(None);
    };
    if hand_off.binding.launch_kind != StartupBindingKind::Successor {
        return Ok(None);
    }
    let attempt = hand_off.binding.attempt;
    let expected = startup_grant_line(&attempt).trim_end().to_owned();
    let stream = open_grant_stream()?;
    read_grant_line(stream, &expected, hand_off.budget_ms).await?;
    let dir = generation_dir.to_path_buf();
    Ok(Some(Box::new(move || {
        Box::pin(async move { verify_grant_file(&dir, &attempt) })
    })))
}

pub fn verify_grant_file(generation_dir: &Path, attempt: &str) -> bool {
    let Ok(text) = std::fs::read_to_string(generation_dir.join(ADOPTION_GRANT_FILE)) else {
        return false;
    };
    let Ok(grant) = serde_json::from_str::<Value>(&text) else {
        return false;
    };
    grant
        .as_object()
        .and_then(|obj| obj.get("attempt"))
        .and_then(Value::as_str)
        == Some(attempt)
}

#[cfg(unix)]
fn open_grant_stream() -> io::Result<tokio::fs::File> {
    let path = PathBuf::from(format!("/dev/fd/{STARTUP_EVENT_FD}"));
    let file = std::fs::OpenOptions::new().read(true).open(path)?;
    Ok(tokio::fs::File::from_std(file))
}

#[cfg(not(unix))]
fn open_grant_stream() -> io::Result<tokio::io::Empty> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "startup pipe grant is only supported on unix",
    ))
}

pub async fn read_grant_line<R: AsyncRead + Unpin>(
    mut stream: R,
    expected: &str,
    budget_ms: u64,
) -> io::Result<()> {
    let read_fut = async {
        let mut pending = String::new();
        let mut buf = [0u8; MAX_STARTUP_EVENT_BYTES];
        loop {
            let bytes = stream.read(&mut buf).await?;
            if bytes == 0 {
                return Err(io::Error::other(
                    "the startup pipe closed before the publication grant",
                ));
            }
            pending.push_str(&String::from_utf8_lossy(&buf[..bytes]));
            while let Some(newline) = pending.find('\n') {
                if &pending[..newline] == expected {
                    return Ok(());
                }
                pending.drain(..=newline);
            }
            if pending.len() > MAX_STARTUP_EVENT_BYTES {
                return Err(io::Error::other("oversized startup grant record"));
            }
        }
    };
    tokio::time::timeout(Duration::from_millis(budget_ms), read_fut)
        .await
        .map_err(|_| io::Error::other(format!("no publication grant within {budget_ms}ms")))?
}

/// Computes filler descriptor numbers needed for `3..STARTUP_EVENT_FD` when spawning a coordinated
/// host child (fd 3 is the watchdog pipe, fd 4 is the presenter channel when `bootstrap` is true).
pub fn host_child_startup_fillers(bootstrap: bool, reports_startup: bool) -> Vec<i32> {
    if !reports_startup {
        return Vec::new();
    }
    let start = if bootstrap { 5 } else { 4 };
    (start..STARTUP_EVENT_FD).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn read_grant_line_accepts_matching_line_and_rejects_oversized_or_eof() {
        let attempt = "att-42";
        let expected = startup_grant_line(attempt).trim_end().to_owned();
        let (mut writer, reader) = tokio::io::duplex(4096);
        writer
            .write_all(b"{\"noise\":true}\n")
            .await
            .expect("noise");
        writer
            .write_all(startup_grant_line(attempt).as_bytes())
            .await
            .expect("grant");
        read_grant_line(reader, &expected, 5_000)
            .await
            .expect("matched");

        let (writer2, reader2) = tokio::io::duplex(4096);
        drop(writer2);
        let eof_err = read_grant_line(reader2, &expected, 5_000)
            .await
            .unwrap_err();
        assert!(eof_err.to_string().contains("closed before"));

        let (mut writer3, reader3) = tokio::io::duplex(16_384);
        writer3
            .write_all("x".repeat(MAX_STARTUP_EVENT_BYTES + 10).as_bytes())
            .await
            .expect("write");
        let over_err = read_grant_line(reader3, &expected, 5_000)
            .await
            .unwrap_err();
        assert!(over_err.to_string().contains("oversized"));
    }

    #[test]
    fn verify_grant_file_and_fillers_match_contract() {
        let temp = tempfile::tempdir().expect("tempdir");
        assert!(!verify_grant_file(temp.path(), "att-1"));
        std::fs::write(
            temp.path().join(ADOPTION_GRANT_FILE),
            "{\"attempt\":\"att-1\"}\n",
        )
        .expect("write");
        assert!(verify_grant_file(temp.path(), "att-1"));
        assert!(!verify_grant_file(temp.path(), "att-2"));

        assert!(host_child_startup_fillers(true, false).is_empty());
        assert_eq!(
            host_child_startup_fillers(true, true),
            (5..16).collect::<Vec<_>>()
        );
        assert_eq!(
            host_child_startup_fillers(false, true),
            (4..16).collect::<Vec<_>>()
        );
    }
}
