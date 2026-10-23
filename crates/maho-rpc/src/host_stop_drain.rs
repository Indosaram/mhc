//! `drain_host`: asking a generation to finish its work and leave (senpi `host-stop-drain.ts`, senpi#3653).
//!
//! A drain ends no work, so it is permitted against any generation whose record PROVES the process
//! serving this endpoint (pid + start time) - the current one by default, or one NAMED by instance id.
//!
//! Two channels exist, chosen on evidence, never on platform:
//! - the drain REQUEST file, used whenever the generation advertises it reads one AND that advertisement
//!   names the very pid just proven. It needs no signal, so it is the only channel a win32 generation has;
//! - SIGUSR1, only where the signal has a handler: POSIX, a host advertising the handoff capability, and
//!   the generation that host actually is. A named generation the endpoint does not answer as is never signalled.
//!
//! Anything else is refused (`drain_unsupported`). `wait_ms` turns the request into "drained and gone": the
//! answer then says whether the process exited within the bound, judged only on positive evidence (the pid
//! is gone, or now names a different process); an identity that cannot be read is not an exit.
use crate::{
    host_daemon_paths::{HostDaemonPaths, generation_paths},
    host_daemon_registration::RegisteredHost,
    host_daemon_state::{parse_json, read_file_or_undefined},
    host_reservations::{process_is_live, read_process_start_time},
};
use serde_json::Value;
use std::{io, path::Path, time::Duration};
/// How often a `wait_ms` drain re-checks whether the generation's process has gone.
pub const EXIT_POLL_MS: u64 = 100;
pub const DEFAULT_DRAIN_PROBE_TIMEOUT_MS: u64 = 10_000;
pub struct DrainHostOptions {
    pub socket: String,
    /// Drain this generation instead of the one the pointer names.
    pub instance_id: Option<String>,
    /// Wait up to this long for the drained generation to exit, and report whether it did.
    pub wait_ms: Option<u64>,
    pub timeout_ms: Option<u64>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DrainHostResult {
    Drained {
        pid: u32,
        instance_id: Option<String>,
        /// Present when the caller waited: whether the generation's process exited within `wait_ms`.
        exited: Option<bool>,
    },
    Refuse {
        reason: &'static str,
    },
}
/// `^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$` and no `..` (senpi `isGenerationId`).
pub fn is_generation_id(instance_id: &str) -> bool {
    let bytes = instance_id.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 128
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        && !instance_id.contains("..")
}
/// One NAMED generation's record, whatever the pointer says. The record proves nothing by itself.
pub fn read_generation_registration(
    paths: &HostDaemonPaths,
    instance_id: &str,
) -> io::Result<Option<RegisteredHost>> {
    let text = read_file_or_undefined(&generation_paths(paths, instance_id).pid_file)?;
    let Some(record) = parse_json(text.as_deref()) else {
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
    let process_start_time = match record.get("processStartTime") {
        Some(Value::Null) => None,
        Some(Value::String(value)) if !value.trim().is_empty() => Some(value.clone()),
        _ => return Ok(None),
    };
    let writer=record.get("writer").filter(|writer|writer.is_object()&&writer["pid"].is_number()).map(|writer|serde_json::json!({"pid":writer["pid"],"startTime":writer["startTime"].as_str()}));
    Ok(Some(RegisteredHost {
        pid,
        process_start_time,
        socket: record
            .get("socket")
            .and_then(Value::as_str)
            .map(str::to_owned),
        instance_id: instance_id.into(),
        generation: record
            .get("generation")
            .and_then(Value::as_f64)
            .unwrap_or(0.),
        writer,
    }))
}
/// The pid a legacy (pre-generation) record proves for this endpoint, when its start time still matches.
pub fn proven_legacy_owner(paths: &HostDaemonPaths, socket: &str) -> Option<u32> {
    let record = crate::host_daemon_registration::read_legacy_host_record(paths)?;
    let pid = record
        .get("pid")
        .and_then(Value::as_u64)
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid > 0)?;
    let recorded = record.get("processStartTime").and_then(Value::as_str)?;
    let same_endpoint = record
        .get("socket")
        .and_then(Value::as_str)
        .is_some_and(|recorded| {
            crate::host_ensure::normalize_socket_path(recorded)
                == crate::host_ensure::normalize_socket_path(socket)
        });
    (same_endpoint && read_process_start_time(pid).as_deref() == Some(recorded)).then_some(pid)
}
/// The advertised watcher pid of a generation's supervisor, or nothing: such a generation cannot be asked this way.
pub fn read_drain_watch(file: &Path) -> Option<u32> {
    let text = std::fs::read_to_string(file).ok()?;
    let parsed: Value = serde_json::from_str(&text).ok()?;
    parsed
        .get("pid")
        .and_then(Value::as_u64)
        .filter(|pid| *pid > 0)
        .and_then(|pid| u32::try_from(pid).ok())
}
/// Records a drain request for whoever polls `file`. Atomic: a reader sees no request or a whole one.
pub fn write_drain_request(
    file: &Path,
    sender_pid: u32,
    sender_kind: &str,
    reason: &str,
) -> io::Result<()> {
    let record = serde_json::json!({"sender":{"pid":sender_pid,"kind":sender_kind},"reason":reason,"at":chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis,true),"nonce":uuid::Uuid::new_v4().to_string()});
    let temp = file.with_extension(format!("{}.tmp", uuid::Uuid::new_v4().simple()));
    std::fs::write(
        &temp,
        serde_json::to_vec(&record).map_err(io::Error::other)?,
    )?;
    std::fs::rename(&temp, file).inspect_err(|_| {
        let _ = std::fs::remove_file(&temp);
    })
}
pub async fn drain_host(
    paths: &HostDaemonPaths,
    options: DrainHostOptions,
) -> io::Result<DrainHostResult> {
    let named = options.instance_id.as_deref();
    if named.is_some_and(|named| !is_generation_id(named)) {
        return Ok(DrainHostResult::Refuse {
            reason: "unknown_owner",
        });
    }
    let registered = match named {
        None => crate::host_daemon_registration::read_host_registration(paths)?,
        Some(named) => read_generation_registration(paths, named)?,
    };
    let owner = crate::host_daemon_registration::proven_owner(registered.as_ref(), &options.socket);
    if let Some(owner) = owner {
        let generation = generation_paths(paths, &owner.instance_id);
        if read_drain_watch(&generation.drain_watch_file) == Some(owner.pid) {
            match write_drain_request(
                &generation.drain_request_file,
                std::process::id(),
                "stop",
                "operator_drain",
            ) {
                Ok(()) => {}
                // The generation ended and released its directory between the ownership check and this write: it is
                // already drained, and the exit check below reports it.
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            return Ok(drained(
                owner.pid,
                owner.process_start_time.as_deref(),
                Some(&owner.instance_id),
                options.wait_ms,
            )
            .await);
        }
    }
    if named.is_some() && owner.is_none() {
        return Ok(DrainHostResult::Refuse {
            reason: "unknown_owner",
        });
    }
    let pid = match owner {
        Some(owner) => Some(owner.pid),
        None => proven_legacy_owner(paths, &options.socket),
    };
    let Some(pid) = pid else {
        return Ok(DrainHostResult::Refuse {
            reason: "unknown_owner",
        });
    };
    let host = crate::host_probe::probe_protocol_info(
        &options.socket,
        options.timeout_ms.unwrap_or(DEFAULT_DRAIN_PROBE_TIMEOUT_MS),
    )
    .await;
    // SIGUSR1 terminates a process that installed no handler for it: only a POSIX host that advertises the drain,
    // and that is the generation being asked, is signalled.
    let answers_as_target = named.is_none_or(|named| {
        host.as_ref().and_then(|host| host.instance_id.as_deref()) == Some(named)
    });
    let capable = host.as_ref().is_some_and(|host| {
        host.capabilities
            .iter()
            .any(|capability| capability == crate::host_decision::GENERATION_HANDOFF_CAPABILITY)
    });
    if !capable || !answers_as_target || cfg!(windows) {
        return Ok(DrainHostResult::Refuse {
            reason: "drain_unsupported",
        });
    }
    signal_drain(pid)?;
    Ok(match owner {
        Some(owner) => {
            drained(
                pid,
                owner.process_start_time.as_deref(),
                Some(&owner.instance_id),
                options.wait_ms,
            )
            .await
        }
        None => drained(pid, None, None, options.wait_ms).await,
    })
}
async fn drained(
    pid: u32,
    start_time: Option<&str>,
    instance_id: Option<&str>,
    wait_ms: Option<u64>,
) -> DrainHostResult {
    let exited = match wait_ms {
        None => None,
        Some(wait_ms) => Some(wait_for_exit(pid, start_time, wait_ms).await),
    };
    DrainHostResult::Drained {
        pid,
        instance_id: instance_id.map(str::to_owned),
        exited,
    }
}
async fn wait_for_exit(pid: u32, start_time: Option<&str>, wait_ms: u64) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(wait_ms);
    loop {
        if exited_now(pid, start_time, process_is_live, read_process_start_time) {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(EXIT_POLL_MS)).await;
    }
}
/// Exit judged only on positive evidence: the pid is gone, or its start time now differs from the recorded one.
/// A live process whose start time cannot be read (`None`) is NOT an exit (senpi `ProcessIdentityUnreadableError`).
pub fn exited_now(
    pid: u32,
    recorded_start: Option<&str>,
    is_live: impl Fn(u32) -> bool,
    read_start: impl Fn(u32) -> Option<String>,
) -> bool {
    if !is_live(pid) {
        return true;
    }
    let Some(recorded) = recorded_start else {
        return false;
    };
    read_start(pid).is_some_and(|live| live != recorded)
}
/// A generation that left between the proof and the signal is already drained (ESRCH).
fn signal_drain(pid: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        crate::host_stop::signal_generation(pid, rustix::process::Signal::USR1)
            .map(|_| ())
            .map_err(io::Error::from)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generation_ids_reject_traversal_and_bad_leading_characters() {
        assert!(is_generation_id("a1b2-c3.d_e"));
        assert!(!is_generation_id(""));
        assert!(!is_generation_id("-a"));
        assert!(!is_generation_id("a..b"));
        assert!(!is_generation_id("a/b"));
        assert!(!is_generation_id(&"a".repeat(129)));
        assert!(is_generation_id(&"a".repeat(128)));
    }
    #[test]
    fn exit_needs_positive_evidence() {
        assert!(exited_now(7, Some("1"), |_| false, |_| None));
        assert!(!exited_now(7, Some("1"), |_| true, |_| None));
        assert!(!exited_now(7, None, |_| true, |_| Some("2".into())));
        assert!(!exited_now(7, Some("1"), |_| true, |_| Some("1".into())));
        assert!(exited_now(7, Some("1"), |_| true, |_| Some("2".into())));
    }
    #[test]
    fn drain_request_round_trips_and_a_missing_directory_is_not_found() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("drain-request.json");
        write_drain_request(&file, 42, "stop", "operator_drain").unwrap();
        let record: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(
            record["sender"],
            serde_json::json!({"pid":42,"kind":"stop"})
        );
        assert_eq!(record["reason"], "operator_drain");
        assert!(record["nonce"].is_string());
        let missing = temp.path().join("gone").join("drain-request.json");
        assert_eq!(
            write_drain_request(&missing, 1, "stop", "x")
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
    }
    #[test]
    fn drain_watch_names_a_positive_pid_only() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("watch");
        assert_eq!(read_drain_watch(&file), None);
        std::fs::write(&file, r#"{"pid":9,"at":"x"}"#).unwrap();
        assert_eq!(read_drain_watch(&file), Some(9));
        std::fs::write(&file, r#"{"pid":0}"#).unwrap();
        assert_eq!(read_drain_watch(&file), None);
    }
    #[tokio::test]
    async fn naming_an_invalid_or_unregistered_generation_refuses_unknown_owner() {
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::host_daemon_paths::create_host_daemon_paths("sock", temp.path());
        for named in ["../x", "nope"] {
            let result = drain_host(
                &paths,
                DrainHostOptions {
                    socket: "sock".into(),
                    instance_id: Some(named.into()),
                    wait_ms: None,
                    timeout_ms: Some(100),
                },
            )
            .await
            .unwrap();
            assert_eq!(
                result,
                DrainHostResult::Refuse {
                    reason: "unknown_owner"
                }
            );
        }
    }
}
