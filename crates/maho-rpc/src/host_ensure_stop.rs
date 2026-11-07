//! HOW an ensure stops a supervisor it is entitled to stop (senpi `rpc/host-ensure-stop.ts`): SIGTERM, a
//! deadline, then SIGKILL - with the stop intent on record before the first signal and, when the SIGKILL was
//! needed, the terminal record written by THIS process, because a SIGKILLed supervisor runs no exit handler
//! (senpi#2566). Split out of `host_ensure.rs`.
use crate::host_daemon_paths::HostGenerationPaths;
use crate::host_stalled_evidence::{active_stop_progress, child_stalled_stop_max_ms, now_ms};
use crate::host_stop_intent::{
    CHILD_KILL_EXIT_TIMEOUT_MS, CHILD_STOP_TIMEOUT_MS, HostStopIntent, HostStopSender,
    read_stop_intent, write_stop_intent,
};
use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;

/// senpi `DEFAULT_STOP_TIMEOUT_MS`, read from the same environment the supervisor bounds its stall wait by.
pub fn default_stop_timeout_ms(env: &HashMap<String, String>) -> u64 {
    CHILD_STOP_TIMEOUT_MS.max(child_stalled_stop_max_ms(env)) + CHILD_KILL_EXIT_TIMEOUT_MS + 5_000
}
pub use crate::deadline::SIGKILL_GRACE_MS;
/// A stop-progress report older than this is a supervisor that stopped reporting, not one still waiting.
const STOP_PROGRESS_FRESH_MS: u64 = 10_000;
/// After a reported stall wait ends, the supervisor still escalates, records and exits: one more window.
const AFTER_STALL_WAIT_GRACE_MS: u64 = CHILD_KILL_EXIT_TIMEOUT_MS + 5_000;
/// The longest a SIGTERM wait here can run (senpi `STOP_WAIT_BUDGET_MS`): the ordinary window, or a supervisor's
/// reported stall wait. Read from the same `SENPI_RPC_CHILD_STALLED_STOP_MAX_MS` the supervisor bounds itself by,
/// so an operator who raises it also raises every lock and deadline sized from this budget.
pub fn stop_wait_budget_ms(env: &HashMap<String, String>) -> u64 {
    default_stop_timeout_ms(env).max(child_stalled_stop_max_ms(env) + AFTER_STALL_WAIT_GRACE_MS)
        + SIGKILL_GRACE_MS
}

/// The generation a stop acts on and who asks (senpi `StopTarget`).
#[derive(Debug, Clone)]
pub struct StopTarget {
    pub daemon_dir: PathBuf,
    pub generation: HostGenerationPaths,
    pub instance_id: String,
    pub sender: HostStopSender,
    pub reason: String,
}

/// A validated pid record: `process_start_time: None` answers only the liveness half (senpi `DaemonPidFile`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonPidFile {
    pub pid: u32,
    pub process_start_time: Option<String>,
}

pub fn ensure_sender() -> HostStopSender {
    HostStopSender {
        pid: std::process::id(),
        kind: "ensure".into(),
        generation: None,
    }
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

pub fn announce_stop(target: &StopTarget, target_pid: u32) -> HostStopIntent {
    let intent = HostStopIntent {
        sender: target.sender.clone(),
        chain: None,
        target_pid,
        reason: target.reason.clone(),
        signal: "SIGTERM".into(),
        at: now_iso(),
    };
    write_stop_intent(&target.generation, &intent, |line| eprintln!("{line}"));
    intent
}

/// Ownership for the reuse decision. An identity we cannot read proves nothing: it can neither claim the host nor
/// authorize a kill, so it reads as "not ours" and the caller starts fresh.
pub fn matches_pid_file_or_unknown(
    pid_file: &DaemonPidFile,
    read_start_time: impl Fn(u32) -> Option<String>,
) -> bool {
    resolve_pid_file_ownership(pid_file, read_start_time) == PidFileOwnership::Owns
}

/// senpi `noteEscalatedStop` (host-child-exit.ts): the terminal record of a generation this caller SIGKILLed;
/// a generation that already has its terminal record keeps the first one (senpi `recordTerminalHostRecord`).
fn note_escalated_stop(daemon_dir: &std::path::Path, instance_id: &str, intent: &HostStopIntent) {
    if crate::host_crash_record::read_host_crash_records(daemon_dir)
        .iter()
        .any(|existing| existing["kind"] == "rpc-host" && existing["generation"] == instance_id)
    {
        return;
    }
    let mut record = serde_json::json!({
        "at": now_iso(),
        "signal": "SIGKILL",
        "kind": "rpc-host",
        "generation": instance_id,
        "detection": "engine_stop",
        "sender": {"pid": intent.sender.pid, "kind": intent.sender.kind},
        "reason": format!("{}; supervisor_escalated", intent.reason),
        "stopIntent": true,
    });
    if let Some(generation) = &intent.sender.generation {
        record["sender"]["generation"] = generation.clone().into();
    }
    if let Some(chain) = &intent.chain {
        record["chain"] = chain
            .iter()
            .map(|sender| serde_json::json!({"pid": sender.pid, "kind": sender.kind}))
            .collect();
    }
    crate::host_crash_record::record_host_crash(daemon_dir, &record);
}

/// The supervisor was SIGKILLed and is gone: record the generation's end before the caller releases the
/// registration that holds the intent. The intent as the supervisor last left it wins - it may have layered its
/// own step on - and the one this caller wrote stands in when the file is gone.
pub fn record_escalation(target: &StopTarget, announced: &HostStopIntent) {
    let current = read_stop_intent(&target.generation, now_ms(), None);
    note_escalated_stop(
        &target.daemon_dir,
        &target.instance_id,
        current.as_ref().unwrap_or(announced),
    );
}

/// Stops a child this process spawned: SIGTERM, the (possibly extended) deadline, then SIGKILL.
pub async fn stop_spawned_child(
    child: &mut tokio::process::Child,
    term_timeout_ms: u64,
    target: &StopTarget,
) -> std::io::Result<()> {
    if child.try_wait()?.is_some() {
        return Ok(());
    }
    let Some(pid) = child.id() else {
        return Ok(());
    };
    let announced = announce_stop(target, pid);
    signal_pid(pid, StopSignal::Term)?;
    let exited = std::sync::Arc::new(tokio::sync::Mutex::new(child));
    let wait_for = |ms: u64| {
        let exited = std::sync::Arc::clone(&exited);
        async move {
            let mut child = exited.lock().await;
            tokio::time::timeout(std::time::Duration::from_millis(ms), child.wait())
                .await
                .is_ok()
        }
    };
    if wait_while_supervisor_reports(target, term_timeout_ms, wait_for).await {
        return Ok(());
    }
    signal_pid(pid, StopSignal::Kill)?;
    if !wait_for(SIGKILL_GRACE_MS).await {
        return Err(std::io::Error::other(format!(
            "RPC socket host pid {pid} remained alive after SIGKILL"
        )));
    }
    record_escalation(target, &announced);
    Ok(())
}

/// Replacing a managed host nothing can reach: only through its validated pidfile (I1).
pub async fn stop_managed_host(
    pid_file: &DaemonPidFile,
    term_timeout_ms: u64,
    target: &StopTarget,
    read_start_time: impl Fn(u32) -> Option<String> + Copy,
) -> std::io::Result<()> {
    let announced = announce_stop(target, pid_file.pid);
    signal_validated(pid_file, StopSignal::Term, read_start_time)?;
    let gone = |ms: u64| wait_for_gone(pid_file, ms, read_start_time);
    if wait_while_supervisor_reports(target, term_timeout_ms, gone).await {
        return Ok(());
    }
    signal_validated(pid_file, StopSignal::Kill, read_start_time)?;
    if !wait_for_gone(pid_file, SIGKILL_GRACE_MS, read_start_time).await {
        return Err(std::io::Error::other(format!(
            "RPC socket host pid {} remained alive after SIGKILL",
            pid_file.pid
        )));
    }
    record_escalation(target, &announced);
    Ok(())
}

/// The caller's SIGTERM deadline, extended while the supervisor reports it is waiting out a stalled child
/// (`stop-progress.json`): killing the supervisor then would SIGKILL the very host its stall wait is protecting.
/// Bounded by the supervisor's own `untilAt` plus a grace, and only while reports stay fresh.
async fn wait_while_supervisor_reports<F, W>(
    target: &StopTarget,
    term_timeout_ms: u64,
    mut wait_for: W,
) -> bool
where
    W: FnMut(u64) -> F,
    F: Future<Output = bool>,
{
    if wait_for(term_timeout_ms).await {
        return true;
    }
    let mut extended = false;
    loop {
        let now = now_ms();
        let Some(until_at) =
            active_stop_progress(&target.generation, now, STOP_PROGRESS_FRESH_MS as i64)
        else {
            return extended && wait_for(AFTER_STALL_WAIT_GRACE_MS).await;
        };
        extended = true;
        let remaining = u64::try_from((until_at - now).max(1)).unwrap_or(1);
        if wait_for(STOP_PROGRESS_FRESH_MS.min(remaining)).await {
            return true;
        }
    }
}

/// senpi `PidFileOwnership`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PidFileOwnership {
    Owns,
    Gone,
    Unknown,
}

/// One probe per call: the teardown loops are themselves the retry. A probe that fails against a LIVE pid is
/// `Unknown` - it proves nothing about ownership, so signalling on it would be unsafe and treating it as gone
/// would abandon a host that may still be running. A failed probe against a dead pid is `Gone`.
pub fn resolve_pid_file_ownership(
    pid_file: &DaemonPidFile,
    read_start_time: impl Fn(u32) -> Option<String>,
) -> PidFileOwnership {
    let live = crate::host_reservations::process_is_live(pid_file.pid);
    let Some(recorded) = pid_file.process_start_time.as_deref() else {
        return if live {
            PidFileOwnership::Unknown
        } else {
            PidFileOwnership::Gone
        };
    };
    match read_start_time(pid_file.pid) {
        Some(current) if current == recorded => PidFileOwnership::Owns,
        Some(_) => PidFileOwnership::Gone,
        None if live => PidFileOwnership::Unknown,
        None => PidFileOwnership::Gone,
    }
}

/// The two signals an ensure sends; Windows terminates on both (Node maps them to TerminateProcess).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopSignal {
    Term,
    Kill,
}

fn signal_validated(
    pid_file: &DaemonPidFile,
    signal: StopSignal,
    read_start_time: impl Fn(u32) -> Option<String>,
) -> std::io::Result<()> {
    if resolve_pid_file_ownership(pid_file, read_start_time) != PidFileOwnership::Owns {
        return Ok(());
    }
    signal_pid(pid_file.pid, signal)
}

async fn wait_for_gone(
    pid_file: &DaemonPidFile,
    timeout_ms: u64,
    read_start_time: impl Fn(u32) -> Option<String> + Copy,
) -> bool {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    crate::deadline::wait_until(deadline, crate::deadline::POLL_INTERVAL, || {
        resolve_pid_file_ownership(pid_file, read_start_time) == PidFileOwnership::Gone
    })
    .await
}

/// ESRCH means the process is already gone, which is what every caller here wants.
pub fn signal_pid(pid: u32, signal: StopSignal) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let signal = match signal {
            StopSignal::Term => rustix::process::Signal::TERM,
            StopSignal::Kill => rustix::process::Signal::KILL,
        };
        crate::host_stop::signal_generation(pid, signal)
            .map(|_| ())
            .map_err(std::io::Error::from)
    }
    #[cfg(windows)]
    {
        let _ = signal;
        crate::host_stop::terminate_generation(pid).map(|_| ())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (pid, signal);
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "signalling hosts is unsupported on this platform",
        ))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::host_daemon_paths::{generation_paths, host_daemon_directory_paths};

    fn target(dir: &std::path::Path) -> StopTarget {
        let paths = host_daemon_directory_paths(dir);
        let generation = generation_paths(&paths, "g");
        std::fs::create_dir_all(&generation.dir).expect("create generation dir");
        StopTarget {
            daemon_dir: dir.to_path_buf(),
            generation,
            instance_id: "g".into(),
            sender: ensure_sender(),
            reason: "replace_unreachable".into(),
        }
    }
    fn dead_pid() -> u32 {
        let mut child = std::process::Command::new("true")
            .spawn()
            .expect("spawn dead child");
        let pid = child.id();
        child.wait().expect("wait for dead child");
        pid
    }

    #[test]
    fn budgets_grow_with_the_stalled_child_bound() {
        let env = HashMap::new();
        assert_eq!(default_stop_timeout_ms(&env), 60_000 + 30_000 + 5_000);
        assert_eq!(stop_wait_budget_ms(&env), 95_000 + SIGKILL_GRACE_MS);
        let raised = HashMap::from([(
            crate::host_stalled_evidence::CHILD_STALLED_STOP_MAX_MS_ENV.to_owned(),
            "200000".to_owned(),
        )]);
        assert_eq!(default_stop_timeout_ms(&raised), 235_000);
        assert_eq!(stop_wait_budget_ms(&raised), 237_000);
        assert_eq!(ensure_sender().kind, "ensure");
        assert_eq!(ensure_sender().pid, std::process::id());
    }

    #[test]
    fn announced_intent_is_on_record_and_escalation_cites_it() {
        let temp = tempfile::tempdir().unwrap();
        let target = target(temp.path());
        let announced = announce_stop(&target, 42);
        let read = read_stop_intent(&target.generation, now_ms(), None).unwrap();
        assert_eq!(read.target_pid, 42);
        assert_eq!(read.reason, "replace_unreachable");
        record_escalation(&target, &announced);
        let records = crate::host_crash_record::read_host_crash_records(temp.path());
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["signal"], "SIGKILL");
        assert_eq!(records[0]["detection"], "engine_stop");
        assert_eq!(records[0]["generation"], "g");
        assert_eq!(
            records[0]["reason"],
            "replace_unreachable; supervisor_escalated"
        );
        // One terminal record per generation: a second escalation keeps the first.
        record_escalation(&target, &announced);
        assert_eq!(
            crate::host_crash_record::read_host_crash_records(temp.path()).len(),
            1
        );
        // The file gone, the announced intent stands in.
        crate::host_stop_intent::clear_stop_intent(&target.generation);
        let other_target = StopTarget {
            instance_id: "h".into(),
            ..target
        };
        let other = HostStopIntent {
            reason: "fallback".into(),
            ..announced
        };
        record_escalation(&other_target, &other);
        let records = crate::host_crash_record::read_host_crash_records(temp.path());
        assert_eq!(records[1]["reason"], "fallback; supervisor_escalated");
    }

    #[test]
    fn ownership_needs_a_matching_start_time_and_an_unreadable_live_one_is_unknown() {
        let own = std::process::id();
        let record = |start: Option<&str>| DaemonPidFile {
            pid: own,
            process_start_time: start.map(str::to_owned),
        };
        assert_eq!(
            resolve_pid_file_ownership(&record(None), |_| None),
            PidFileOwnership::Unknown
        );
        assert_eq!(
            resolve_pid_file_ownership(&record(Some("1")), |_| Some("1".into())),
            PidFileOwnership::Owns
        );
        assert_eq!(
            resolve_pid_file_ownership(&record(Some("1")), |_| Some("2".into())),
            PidFileOwnership::Gone
        );
        assert_eq!(
            resolve_pid_file_ownership(&record(Some("1")), |_| None),
            PidFileOwnership::Unknown
        );
        assert!(!matches_pid_file_or_unknown(&record(Some("1")), |_| None));
        assert!(matches_pid_file_or_unknown(&record(Some("1")), |_| Some(
            "1".into()
        )));
        let dead = DaemonPidFile {
            pid: dead_pid(),
            process_start_time: None,
        };
        assert_eq!(
            resolve_pid_file_ownership(&dead, |_| None),
            PidFileOwnership::Gone
        );
    }

    #[test]
    fn signalling_a_gone_pid_is_not_an_error() {
        signal_pid(dead_pid(), StopSignal::Term).unwrap();
    }

    #[tokio::test]
    async fn a_gone_host_stops_without_escalating() {
        let temp = tempfile::tempdir().unwrap();
        let target = target(temp.path());
        let pid_file = DaemonPidFile {
            pid: dead_pid(),
            process_start_time: Some("1".into()),
        };
        stop_managed_host(&pid_file, 1_000, &target, |_| None)
            .await
            .unwrap();
        assert!(crate::host_crash_record::read_host_crash_records(temp.path()).is_empty());
        assert_eq!(
            read_stop_intent(&target.generation, now_ms(), None)
                .unwrap()
                .target_pid,
            pid_file.pid
        );
    }

    #[tokio::test]
    async fn a_fresh_stall_report_extends_the_term_deadline() {
        let temp = tempfile::tempdir().unwrap();
        let target = target(temp.path());
        let mut asked = Vec::new();
        let no_report = wait_while_supervisor_reports(&target, 7, |ms| {
            asked.push(ms);
            async { false }
        })
        .await;
        assert!(!no_report);
        assert_eq!(asked, vec![7]);
        let now = chrono::Utc::now();
        crate::host_stalled_evidence::write_stop_progress(
            &target.generation,
            &crate::host_stalled_evidence::StopProgress {
                at: now.to_rfc3339(),
                until_at: (now + chrono::Duration::seconds(60)).to_rfc3339(),
            },
        )
        .unwrap();
        let mut asked = Vec::new();
        let extended = wait_while_supervisor_reports(&target, 7, |ms| {
            asked.push(ms);
            let done = asked.len() == 2;
            async move { done }
        })
        .await;
        assert!(extended);
        assert_eq!(asked, vec![7, STOP_PROGRESS_FRESH_MS]);
    }

    #[tokio::test]
    async fn a_spawned_child_stops_on_sigterm() {
        let temp = tempfile::tempdir().unwrap();
        let target = target(temp.path());
        let mut child = tokio::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        stop_spawned_child(&mut child, 10_000, &target)
            .await
            .unwrap();
        assert!(child.try_wait().unwrap().is_some());
        assert!(crate::host_crash_record::read_host_crash_records(temp.path()).is_empty());
    }
}
