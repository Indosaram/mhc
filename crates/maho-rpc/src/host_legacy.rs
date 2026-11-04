//! A host from before layout 2: the process a FLAT `<agentDir>/rpc-host-daemon/host.pid` names (senpi
//! `rpc/host-legacy.ts`). This build never writes and never removes that file. Two acts are allowed on its
//! strength, both only while the record's identity guard (pid plus start time) still matches the live
//! process: a DRAIN (SIGUSR1) of a host advertising `generation_handoff`, and an ensure's takeover of such a
//! host when it reports no session at all.
//!
//! An unstamped record belongs to the agent directory's default socket, the only endpoint those writers
//! kept in the flat directory.
use crate::host_daemon_paths::HostDaemonPaths;
use crate::host_daemon_state::{parse_json, read_file_or_undefined};
use crate::host_decision::GENERATION_HANDOFF_CAPABILITY;
use crate::host_probe::{probe_protocol_info, probe_session_count};
use crate::host_reservations::process_is_live;
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LegacyHostRecord {
    pub pid: u32,
    pub process_start_time: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LegacyHost {
    pub record: LegacyHostRecord,
    pub socket: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum LegacyHostVerdict {
    Absent,
    Idle(LegacyHost),
    Held(String),
}

const LEGACY_PROBE_TIMEOUT_MS: u64 = 10_000;
pub const DEFAULT_LEGACY_DRAIN_TIMEOUT_MS: u64 = LEGACY_PROBE_TIMEOUT_MS;
const DRAIN_POLL_MS: u64 = 50;

/// An ensure's whole dealing with a legacy host: nothing to do, or drain an idle one and wait until it is
/// gone. Returns the refusal detail when the ensure must refuse `legacy_host` instead. `probe(pid)` reads a
/// live process's start time (production: `read_process_start_time`).
pub async fn retire_idle_legacy_host<P>(
    paths: &HostDaemonPaths,
    probe: P,
    drain_timeout_ms: u64,
) -> std::io::Result<Option<String>>
where
    P: Fn(u32) -> Option<String>,
{
    let host = match judge_legacy_host(paths, &probe).await? {
        LegacyHostVerdict::Absent => return Ok(None),
        LegacyHostVerdict::Held(detail) => return Ok(Some(detail)),
        LegacyHostVerdict::Idle(host) => host,
    };
    if drain_and_wait(&host.record, &probe, drain_timeout_ms, send_drain).await? {
        return Ok(None);
    }
    Ok(Some(format!(
        "pid {} ({}) was asked to drain and is still running after {drain_timeout_ms}ms; retry once it exits",
        host.record.pid, host.socket
    )))
}

pub fn read_legacy_host(paths: &HostDaemonPaths) -> std::io::Result<Option<LegacyHost>> {
    let Some(record) = parse_json(read_file_or_undefined(&paths.legacy_pid_file)?.as_deref())
    else {
        return Ok(None);
    };
    let Some(pid) = record
        .get("pid")
        .and_then(Value::as_u64)
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid > 0)
    else {
        return Ok(None);
    };
    let socket = record
        .get("socket")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| {
            let agent_dir = paths.flat_dir.parent().unwrap_or(&paths.flat_dir);
            agent_dir
                .join("rpc")
                .join("rpc.sock")
                .to_string_lossy()
                .into_owned()
        });
    Ok(Some(LegacyHost {
        record: LegacyHostRecord {
            pid,
            process_start_time: record
                .get("processStartTime")
                .and_then(Value::as_str)
                .map(str::to_owned),
        },
        socket,
    }))
}

/// How the record's identity guard stands: `None` is a live pid whose start time cannot be read.
fn record_matches(
    record: &LegacyHostRecord,
    probe: &impl Fn(u32) -> Option<String>,
) -> Option<bool> {
    if !process_is_live(record.pid) {
        return Some(false);
    }
    let live = probe(record.pid)?;
    Some(
        record
            .process_start_time
            .as_deref()
            .is_none_or(|recorded| recorded == live),
    )
}

/// Whether an ensure may retire the legacy host: `Idle` only when the process is proven, its endpoint
/// answers with the drain capability, and the host lists no session. Nothing unproven is ever signalled.
async fn judge_legacy_host(
    paths: &HostDaemonPaths,
    probe: &impl Fn(u32) -> Option<String>,
) -> std::io::Result<LegacyHostVerdict> {
    let Some(host) = read_legacy_host(paths)? else {
        return Ok(LegacyHostVerdict::Absent);
    };
    let pid = host.record.pid;
    let place = format!("pid {pid} ({})", host.socket);
    match record_matches(&host.record, probe) {
        Some(false) => return Ok(LegacyHostVerdict::Absent),
        None => {
            crate::host_supervisor_log::log_unknown_host_identity("legacy_host.pid", pid);
            return Ok(LegacyHostVerdict::Held(format!(
                "{place} cannot be proven to be the process its record names; stop it by hand"
            )));
        }
        Some(true) => {}
    }
    if cfg!(windows) {
        return Ok(LegacyHostVerdict::Held(format!(
            "{place} cannot be drained on win32; it exits once idle, then retry"
        )));
    }
    let protocol = probe_protocol_info(&host.socket, LEGACY_PROBE_TIMEOUT_MS).await;
    if !protocol.as_ref().is_some_and(|protocol| {
        protocol
            .capabilities
            .iter()
            .any(|capability| capability == GENERATION_HANDOFF_CAPABILITY)
    }) {
        let why = if protocol.is_some() {
            "cannot drain"
        } else {
            "does not answer"
        };
        return Ok(LegacyHostVerdict::Held(format!(
            "{place} {why}; it is never signalled - stop it by hand, then retry"
        )));
    }
    let sessions = probe_session_count(&host.socket, LEGACY_PROBE_TIMEOUT_MS).await;
    if sessions == Some(0) {
        return Ok(LegacyHostVerdict::Idle(host));
    }
    Ok(LegacyHostVerdict::Held(busy_legacy_host_detail(
        pid,
        &host.socket,
        sessions,
    )))
}

/// Why a proven legacy host that holds work is left alone, and the command that retires it.
pub fn busy_legacy_host_detail(pid: u32, socket: &str, sessions: Option<usize>) -> String {
    format!(
        "pid {pid} ({socket}) holds {}; run `{} host stop --drain --socket {socket}` to let it finish that work and exit, then retry",
        describe_sessions(sessions),
        maho_core::config::app_name()
    )
}

pub fn describe_sessions(sessions: Option<usize>) -> String {
    match sessions {
        None => "an unknown number of sessions".into(),
        Some(count) => format!("{count} open session{}", if count == 1 { "" } else { "s" }),
    }
}

#[cfg(unix)]
fn send_drain(pid: u32) -> std::io::Result<()> {
    // A host that exits before the signal lands has already done what was asked (ESRCH).
    crate::host_stop::signal_generation(pid, rustix::process::Signal::USR1)
        .map(|_| ())
        .map_err(std::io::Error::from)
}
#[cfg(not(unix))]
fn send_drain(_pid: u32) -> std::io::Result<()> {
    Err(std::io::Error::other(
        "a legacy host cannot be drained on this platform",
    ))
}

/// SIGUSR1, then wait for the proven process to leave. While waiting, an unreadable identity is NOT gone.
async fn drain_and_wait(
    record: &LegacyHostRecord,
    probe: &impl Fn(u32) -> Option<String>,
    timeout_ms: u64,
    signal: impl FnOnce(u32) -> std::io::Result<()>,
) -> std::io::Result<bool> {
    signal(record.pid)?;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    while record_matches(record, probe) != Some(false) {
        if tokio::time::Instant::now() > deadline {
            return Ok(false);
        }
        tokio::time::sleep(std::time::Duration::from_millis(DRAIN_POLL_MS)).await;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(dir: &std::path::Path) -> HostDaemonPaths {
        crate::host_daemon_paths::create_host_daemon_paths(
            &dir.join("rpc/rpc.sock").to_string_lossy(),
            dir,
        )
    }

    #[test]
    fn records_are_read_with_the_default_socket_when_unstamped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = paths(dir.path());
        assert_eq!(read_legacy_host(&paths).expect("read"), None);
        std::fs::create_dir_all(&paths.flat_dir).expect("mkdir");
        std::fs::write(
            &paths.legacy_pid_file,
            r#"{"pid":42,"processStartTime":"7"}"#,
        )
        .expect("write");
        let host = read_legacy_host(&paths).expect("read").expect("host");
        assert_eq!(
            host.record,
            LegacyHostRecord {
                pid: 42,
                process_start_time: Some("7".into())
            }
        );
        assert_eq!(
            host.socket,
            dir.path().join("rpc").join("rpc.sock").to_string_lossy()
        );
        std::fs::write(&paths.legacy_pid_file, r#"{"pid":42,"socket":"/s"}"#).expect("write");
        assert_eq!(
            read_legacy_host(&paths)
                .expect("read")
                .expect("host")
                .socket,
            "/s"
        );
    }

    #[tokio::test]
    async fn a_dead_or_recycled_record_is_absent_and_an_unreadable_one_is_held() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = paths(dir.path());
        std::fs::create_dir_all(&paths.flat_dir).expect("mkdir");
        let me = std::process::id();
        std::fs::write(
            &paths.legacy_pid_file,
            format!(r#"{{"pid":{me},"processStartTime":"1"}}"#),
        )
        .expect("write");
        assert_eq!(
            judge_legacy_host(&paths, &|_| Some("2".into()))
                .await
                .expect("judge"),
            LegacyHostVerdict::Absent
        );
        assert!(matches!(
            judge_legacy_host(&paths, &|_| None).await.expect("judge"),
            LegacyHostVerdict::Held(detail) if detail.ends_with("cannot be proven to be the process its record names; stop it by hand")
        ));
        assert_eq!(
            retire_idle_legacy_host(&paths, |_| Some("2".into()), 10)
                .await
                .expect("retire"),
            None
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_drained_host_that_stays_is_reported() {
        let record = LegacyHostRecord {
            pid: std::process::id(),
            process_start_time: Some("1".into()),
        };
        let signalled = std::cell::Cell::new(0);
        let still = drain_and_wait(&record, &|_| Some("1".into()), 200, |pid| {
            signalled.set(pid);
            Ok(())
        })
        .await
        .expect("drain");
        assert!(!still);
        assert_eq!(signalled.get(), std::process::id());
        let gone = drain_and_wait(&record, &|_| Some("other".into()), 200, |_| Ok(()))
            .await
            .expect("drain");
        assert!(gone);
    }

    #[test]
    fn session_counts_are_described() {
        assert_eq!(describe_sessions(None), "an unknown number of sessions");
        assert_eq!(describe_sessions(Some(1)), "1 open session");
        assert_eq!(describe_sessions(Some(3)), "3 open sessions");
        assert!(
            busy_legacy_host_detail(5, "/s", Some(2)).contains("host stop --drain --socket /s")
        );
    }
}
