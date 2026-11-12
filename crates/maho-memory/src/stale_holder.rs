//! Who holds the reflection lock, decided fail-closed.
//!
//! Behaviour spec: `docs/cleanroom/specs/memory-stale-holder.md`. This decision drives both stale-lock
//! recovery and the doctor report, so being wrong is expensive in both directions: reclaiming a live
//! run's lock corrupts two reflections at once, and refusing a dead one's lock blocks reflection forever.
//!
//! Every uncertainty resolves to `Stuck`, never `Dead`. A process on another host cannot be probed from
//! here, an unreadable file may be mid-write, and a failing probe is not evidence of absence. `Dead`
//! requires proving all of: the launcher on this host is gone (or its pid was reused), any publish
//! claimant is gone, and the run recorded a complete, valid outcome.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReflectionLockVerdict {
    None,
    Live { run_id: String },
    Dead { run_id: String },
    Stuck { run_id: String, reason: String },
}

/// A failed read. `code` mirrors the Node error code the caller distinguishes: `ENOENT` means absent,
/// anything else means the file exists but cannot be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockReadError {
    pub code: String,
}

impl LockReadError {
    pub fn new(code: impl Into<String>) -> Self {
        Self { code: code.into() }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessLiveness {
    Alive,
    Dead,
    Unknown,
}

pub trait ReflectionLockProbe {
    fn read_text(&self, path: &Path) -> Result<String, LockReadError>;
    fn hostname(&self) -> String;
    fn pid_liveness(&self, pid: u32) -> ProcessLiveness;
    fn process_start_identity(&self, pid: u32) -> Result<Option<String>, String>;
}

pub struct SystemReflectionLockProbe;

impl ReflectionLockProbe for SystemReflectionLockProbe {
    fn read_text(&self, path: &Path) -> Result<String, LockReadError> {
        std::fs::read_to_string(path).map_err(|error| LockReadError::new(io_code(&error)))
    }

    fn hostname(&self) -> String {
        crate::host_identity::hostname()
    }

    fn pid_liveness(&self, pid: u32) -> ProcessLiveness {
        if pid == 0 {
            return ProcessLiveness::Unknown;
        }
        #[cfg(target_os = "linux")]
        {
            // A zombie still has /proc/<pid> and still answers `kill -0`, so the two agree here.
            if Path::new(&format!("/proc/{pid}")).exists() {
                ProcessLiveness::Alive
            } else {
                ProcessLiveness::Dead
            }
        }
        // A signal-0 send performs only an existence/permission check; no signal is delivered.
        // `nix` wraps the syscall safely, so this crate keeps `unsafe_code = "forbid"`.
        #[cfg(all(unix, not(target_os = "linux")))]
        {
            let Ok(raw) = i32::try_from(pid) else {
                return ProcessLiveness::Unknown;
            };
            match nix::sys::signal::kill(nix::unistd::Pid::from_raw(raw), None) {
                Ok(()) => ProcessLiveness::Alive,
                Err(nix::errno::Errno::ESRCH) => ProcessLiveness::Dead,
                // The process exists but belongs to another user.
                Err(nix::errno::Errno::EPERM) => ProcessLiveness::Alive,
                Err(_) => ProcessLiveness::Unknown,
            }
        }
        #[cfg(windows)]
        {
            match windows_process_liveness(pid) {
                Some(true) => ProcessLiveness::Alive,
                Some(false) => ProcessLiveness::Dead,
                None => ProcessLiveness::Unknown,
            }
        }
        #[cfg(not(any(unix, windows)))]
        {
            ProcessLiveness::Unknown
        }
    }

    fn process_start_identity(&self, pid: u32) -> Result<Option<String>, String> {
        use crate::lock_record::LockHost;
        Ok(crate::host_identity::SystemLockHost.process_start(pid))
    }
}

/// `OpenProcess` + `WaitForSingleObject`: `Some(true)` while the process runs, `Some(false)` once it
/// has exited or never existed, `None` when access is denied (an elevated process we can see but not
/// signal) or the wait itself fails. `winsafe` wraps both calls safely, so this crate keeps
/// `unsafe_code = "forbid"`.
#[cfg(windows)]
fn windows_process_liveness(pid: u32) -> Option<bool> {
    use winsafe::{HPROCESS, co};

    let process = match HPROCESS::OpenProcess(
        co::PROCESS::QUERY_LIMITED_INFORMATION | co::PROCESS::SYNCHRONIZE,
        false,
        pid,
    ) {
        Ok(process) => process,
        // Access denied means the process exists but is not ours to inspect.
        Err(co::ERROR::ACCESS_DENIED) => return Some(true),
        // A pid that never existed (or the idle process) is rejected as an invalid parameter.
        Err(co::ERROR::INVALID_PARAMETER) => return Some(false),
        Err(_) => return None,
    };
    match process.WaitForSingleObject(Some(0)) {
        Ok(co::WAIT::OBJECT_0) => Some(false),
        Ok(co::WAIT::TIMEOUT) => Some(true),
        _ => None,
    }
}

fn io_code(error: &std::io::Error) -> String {
    match error.kind() {
        std::io::ErrorKind::NotFound => "ENOENT".to_owned(),
        std::io::ErrorKind::PermissionDenied => "EACCES".to_owned(),
        other => format!("{other:?}"),
    }
}

enum JsonRead {
    Missing,
    Unreadable(String),
    Invalid,
    Found(Value),
}

fn read_json(probe: &dyn ReflectionLockProbe, path: &Path) -> JsonRead {
    let text = match probe.read_text(path) {
        Ok(text) => text,
        Err(error) => {
            return if error.code == "ENOENT" {
                JsonRead::Missing
            } else {
                JsonRead::Unreadable(error.code)
            };
        }
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(value) => JsonRead::Found(value),
        Err(_) => JsonRead::Invalid,
    }
}

fn fields_of(value: &Value) -> Option<&Map<String, Value>> {
    value.as_object()
}

/// Whether `value` is a complete recorded outcome **for this run**.
///
/// A file that does not match this shape, or names another run, is not an outcome: finalising on it
/// would settle the wrong run.
fn is_run_outcome(value: &Value, run_id: &str) -> bool {
    let Some(fields) = fields_of(value) else {
        return false;
    };
    let Some(child_exit) = fields.get("childExit").and_then(fields_of) else {
        return false;
    };
    let version_is_one = fields
        .get("version")
        .and_then(Value::as_f64)
        .is_some_and(|version| version == 1.0);
    let names_run = fields.get("runId").and_then(Value::as_str) == Some(run_id);
    let finished = fields.get("finishedAt").is_some_and(Value::is_string);
    let attempt_ok = match fields.get("attempt") {
        None => true,
        Some(attempt) => attempt
            .as_f64()
            .is_some_and(|value| value.fract() == 0.0 && value > 0.0),
    };
    let code_ok = match child_exit.get("code") {
        Some(Value::Null) => true,
        Some(Value::Number(number)) => number.as_f64().is_some(),
        _ => false,
    };
    let signal_ok = matches!(
        child_exit.get("signal"),
        Some(Value::Null) | Some(Value::String(_))
    );
    let timed_out_ok = fields.get("timedOut").is_some_and(Value::is_boolean);
    version_is_one && names_run && finished && attempt_ok && code_ok && signal_ok && timed_out_ok
}

fn stuck(run_id: &str, reason: impl Into<String>) -> ReflectionLockVerdict {
    ReflectionLockVerdict::Stuck {
        run_id: run_id.to_owned(),
        reason: reason.into(),
    }
}

fn pid_u32(value: f64) -> Option<u32> {
    if value >= 0.0 && value.fract() == 0.0 && value <= f64::from(u32::MAX) {
        Some(value as u32)
    } else {
        None
    }
}

/// Split a recorded start identity into its scheme, so two identities are only compared when the same
/// probe produced them. `ps-lstart:…` and `windows-process-creation-filetime:…` describe different
/// quantities and are never evidence about each other.
fn identity_scheme(identity: &str) -> Option<&str> {
    match identity.find(':') {
        Some(0) | None => None,
        Some(index) => Some(&identity[..index]),
    }
}

/// A live pid counts as gone when its recorded start identity proves the pid was reused. An unreadable
/// or incomparable start keeps it live: a reused pid is the only thing that may override a live probe.
fn launcher_liveness(
    pid: f64,
    recorded_start: Option<&str>,
    probe: &dyn ReflectionLockProbe,
) -> ProcessLiveness {
    let Some(pid_value) = pid_u32(pid) else {
        return ProcessLiveness::Unknown;
    };
    let liveness = probe.pid_liveness(pid_value);
    if liveness != ProcessLiveness::Alive {
        return liveness;
    }
    let Some(recorded) = recorded_start else {
        return liveness;
    };
    let actual = probe.process_start_identity(pid_value).unwrap_or(None);
    match actual {
        Some(actual)
            if identity_scheme(recorded).is_some()
                && identity_scheme(recorded) == identity_scheme(&actual)
                && recorded != actual =>
        {
            ProcessLiveness::Dead
        }
        _ => ProcessLiveness::Alive,
    }
}

pub fn classify_reflection_lock_holder(
    reflection_dir: &Path,
    probe: &dyn ReflectionLockProbe,
) -> ReflectionLockVerdict {
    let active = match read_json(probe, &reflection_dir.join("active.lock")) {
        JsonRead::Missing => return ReflectionLockVerdict::None,
        JsonRead::Unreadable(code) => {
            return stuck("unknown", format!("active.lock could not be read ({code})"));
        }
        JsonRead::Invalid => return stuck("unknown", "active.lock is not valid JSON"),
        JsonRead::Found(value) => value,
    };
    let fields = fields_of(&active);
    let run_id = fields
        .and_then(|fields| fields.get("runId"))
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_owned();
    let launcher_pid = fields
        .and_then(|fields| fields.get("launcherPid"))
        .and_then(Value::as_f64);
    let launcher_hostname = fields
        .and_then(|fields| fields.get("launcherHostname"))
        .and_then(Value::as_str);
    let (Some(launcher_pid), Some(launcher_hostname)) = (launcher_pid, launcher_hostname) else {
        return stuck(&run_id, "its launcher identity is missing");
    };
    if launcher_hostname != probe.hostname() {
        return stuck(
            &run_id,
            format!("it is owned by host {launcher_hostname}, which cannot be checked from here"),
        );
    }
    let recorded_start = fields
        .and_then(|fields| fields.get("launcherProcessStart"))
        .and_then(Value::as_str);
    match launcher_liveness(launcher_pid, recorded_start, probe) {
        ProcessLiveness::Alive => return ReflectionLockVerdict::Live { run_id },
        ProcessLiveness::Unknown => {
            return stuck(
                &run_id,
                format!("launcher pid {launcher_pid} could not be checked"),
            );
        }
        ProcessLiveness::Dead => {}
    }

    let run_dir: PathBuf = reflection_dir.join("runs").join(&run_id);
    let outcome = match read_json(probe, &run_dir.join("outcome.json")) {
        JsonRead::Missing => {
            return stuck(
                &run_id,
                format!("launcher pid {launcher_pid} is gone and the run recorded no outcome"),
            );
        }
        JsonRead::Unreadable(code) => {
            return stuck(&run_id, format!("its outcome could not be read ({code})"));
        }
        JsonRead::Invalid => return stuck(&run_id, "its outcome is not valid JSON"),
        JsonRead::Found(value) => value,
    };
    if !is_run_outcome(&outcome, &run_id) {
        return stuck(&run_id, "its outcome is not a recorded run outcome");
    }

    match read_json(probe, &run_dir.join("terminal-claim.json")) {
        JsonRead::Unreadable(code) => {
            return stuck(
                &run_id,
                format!("its terminal claim could not be read ({code})"),
            );
        }
        JsonRead::Invalid => return stuck(&run_id, "its terminal claim is not valid JSON"),
        JsonRead::Found(value) => {
            let claimant_pid = fields_of(&value)
                .and_then(|fields| fields.get("claimantPid"))
                .and_then(Value::as_f64);
            let Some(claimant_pid) = claimant_pid else {
                return stuck(&run_id, "its terminal claim names no claimant");
            };
            match pid_u32(claimant_pid)
                .map_or(ProcessLiveness::Unknown, |pid| probe.pid_liveness(pid))
            {
                ProcessLiveness::Alive => return ReflectionLockVerdict::Live { run_id },
                ProcessLiveness::Unknown => {
                    return stuck(
                        &run_id,
                        format!("publish claimant pid {claimant_pid} could not be checked"),
                    );
                }
                ProcessLiveness::Dead => {}
            }
        }
        JsonRead::Missing => {}
    }
    ReflectionLockVerdict::Dead { run_id }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct Fake {
        files: BTreeMap<String, String>,
        hostname: String,
        alive: Vec<u32>,
        starts: BTreeMap<u32, String>,
        start_error: bool,
    }

    impl Fake {
        fn on_this_host() -> Self {
            Self {
                hostname: "host-a".to_owned(),
                ..Self::default()
            }
        }

        fn with(mut self, name: &str, body: &str) -> Self {
            self.files.insert(name.to_owned(), body.to_owned());
            self
        }

        /// Fixture keys are POSIX-style (`runs/r1/outcome.json`) while a Windows path joins with `\`,
        /// so both sides are compared as normalized forward-slash text.
        fn key(path: &Path) -> String {
            let text = path.to_string_lossy().replace('\\', "/");
            let trimmed = text.strip_prefix("/refl").unwrap_or(&text);
            trimmed.trim_start_matches('/').to_owned()
        }
    }

    impl ReflectionLockProbe for Fake {
        fn read_text(&self, path: &Path) -> Result<String, LockReadError> {
            self.files
                .get(&Self::key(path))
                .cloned()
                .ok_or_else(|| LockReadError::new("ENOENT"))
        }

        fn hostname(&self) -> String {
            self.hostname.clone()
        }

        fn pid_liveness(&self, pid: u32) -> ProcessLiveness {
            if pid == 0 {
                return ProcessLiveness::Unknown;
            }
            if self.alive.contains(&pid) {
                ProcessLiveness::Alive
            } else {
                ProcessLiveness::Dead
            }
        }

        fn process_start_identity(&self, pid: u32) -> Result<Option<String>, String> {
            if self.start_error {
                return Err("probe failed".to_owned());
            }
            Ok(self.starts.get(&pid).cloned())
        }
    }

    const OUTCOME: &str = r#"{"version":1,"runId":"r1","finishedAt":"2026-01-01T00:00:00Z","childExit":{"code":0,"signal":null},"timedOut":false}"#;

    fn lock(pid: u32, host: &str) -> String {
        format!(r#"{{"runId":"r1","launcherPid":{pid},"launcherHostname":"{host}"}}"#)
    }

    #[test]
    fn no_lock_file_means_no_holder() {
        let verdict = classify_reflection_lock_holder(Path::new("/refl"), &Fake::on_this_host());
        assert_eq!(verdict, ReflectionLockVerdict::None);
    }

    #[test]
    fn a_live_launcher_is_live() {
        let probe = Fake::on_this_host().with("active.lock", &lock(42, "host-a"));
        let probe = Fake {
            alive: vec![42],
            ..probe
        };
        let verdict = classify_reflection_lock_holder(Path::new("/refl"), &probe);
        assert_eq!(
            verdict,
            ReflectionLockVerdict::Live {
                run_id: "r1".to_owned()
            }
        );
    }

    #[test]
    fn a_dead_launcher_with_a_valid_outcome_is_dead() {
        let probe = Fake::on_this_host()
            .with("active.lock", &lock(42, "host-a"))
            .with("runs/r1/outcome.json", OUTCOME);
        let verdict = classify_reflection_lock_holder(Path::new("/refl"), &probe);
        assert_eq!(
            verdict,
            ReflectionLockVerdict::Dead {
                run_id: "r1".to_owned()
            }
        );
    }

    #[test]
    fn a_dead_launcher_without_an_outcome_is_stuck() {
        let probe = Fake::on_this_host().with("active.lock", &lock(42, "host-a"));
        let verdict = classify_reflection_lock_holder(Path::new("/refl"), &probe);
        assert!(matches!(verdict, ReflectionLockVerdict::Stuck { .. }));
    }

    #[test]
    fn another_host_is_never_dead() {
        let probe = Fake::on_this_host()
            .with("active.lock", &lock(42, "host-b"))
            .with("runs/r1/outcome.json", OUTCOME);
        let verdict = classify_reflection_lock_holder(Path::new("/refl"), &probe);
        match verdict {
            ReflectionLockVerdict::Stuck { reason, .. } => {
                assert!(reason.contains("host-b"), "{reason}");
            }
            other => panic!("expected stuck, got {other:?}"),
        }
    }

    #[test]
    fn a_reused_pid_counts_as_dead() {
        let mut probe = Fake::on_this_host()
            .with(
                "active.lock",
                r#"{"runId":"r1","launcherPid":42,"launcherHostname":"host-a","launcherProcessStart":"ps-lstart:old"}"#,
            )
            .with("runs/r1/outcome.json", OUTCOME);
        probe.alive = vec![42];
        probe.starts.insert(42, "ps-lstart:new".to_owned());
        let verdict = classify_reflection_lock_holder(Path::new("/refl"), &probe);
        assert_eq!(
            verdict,
            ReflectionLockVerdict::Dead {
                run_id: "r1".to_owned()
            }
        );
    }

    #[test]
    fn an_incomparable_start_keeps_a_live_pid_live() {
        let mut probe = Fake::on_this_host()
            .with(
                "active.lock",
                r#"{"runId":"r1","launcherPid":42,"launcherHostname":"host-a","launcherProcessStart":"ps-lstart:old"}"#,
            )
            .with("runs/r1/outcome.json", OUTCOME);
        probe.alive = vec![42];
        probe
            .starts
            .insert(42, "windows-process-creation-filetime:9".to_owned());
        let verdict = classify_reflection_lock_holder(Path::new("/refl"), &probe);
        assert_eq!(
            verdict,
            ReflectionLockVerdict::Live {
                run_id: "r1".to_owned()
            }
        );
    }

    #[test]
    fn pid_zero_is_never_evidence_of_death() {
        let probe = Fake::on_this_host()
            .with("active.lock", &lock(0, "host-a"))
            .with("runs/r1/outcome.json", OUTCOME);
        assert!(matches!(
            classify_reflection_lock_holder(Path::new("/refl"), &probe),
            ReflectionLockVerdict::Stuck { .. }
        ));
    }

    #[test]
    fn a_failing_start_probe_keeps_a_live_pid_live() {
        let mut probe = Fake::on_this_host()
            .with(
                "active.lock",
                r#"{"runId":"r1","launcherPid":42,"launcherHostname":"host-a","launcherProcessStart":"ps-lstart:old"}"#,
            )
            .with("runs/r1/outcome.json", OUTCOME);
        probe.alive = vec![42];
        probe.start_error = true;
        let verdict = classify_reflection_lock_holder(Path::new("/refl"), &probe);
        assert!(matches!(verdict, ReflectionLockVerdict::Live { .. }));
    }

    #[test]
    fn an_invalid_lock_file_is_stuck() {
        let probe = Fake::on_this_host().with("active.lock", "not json");
        assert!(matches!(
            classify_reflection_lock_holder(Path::new("/refl"), &probe),
            ReflectionLockVerdict::Stuck { .. }
        ));
    }

    #[test]
    fn a_missing_launcher_identity_is_stuck() {
        let probe = Fake::on_this_host().with("active.lock", r#"{"runId":"r1"}"#);
        let verdict = classify_reflection_lock_holder(Path::new("/refl"), &probe);
        match verdict {
            ReflectionLockVerdict::Stuck { run_id, .. } => assert_eq!(run_id, "r1"),
            other => panic!("expected stuck, got {other:?}"),
        }
    }

    #[test]
    fn an_outcome_naming_another_run_is_stuck() {
        let other = OUTCOME.replace("r1", "r2");
        let probe = Fake::on_this_host()
            .with("active.lock", &lock(42, "host-a"))
            .with("runs/r1/outcome.json", &other);
        assert!(matches!(
            classify_reflection_lock_holder(Path::new("/refl"), &probe),
            ReflectionLockVerdict::Stuck { .. }
        ));
    }

    #[test]
    fn an_incomplete_outcome_is_stuck() {
        let partial = r#"{"version":1,"runId":"r1","childExit":{"code":0,"signal":null}}"#;
        let probe = Fake::on_this_host()
            .with("active.lock", &lock(42, "host-a"))
            .with("runs/r1/outcome.json", partial);
        assert!(matches!(
            classify_reflection_lock_holder(Path::new("/refl"), &probe),
            ReflectionLockVerdict::Stuck { .. }
        ));
    }

    #[test]
    fn a_live_publish_claimant_is_live() {
        let mut probe = Fake::on_this_host()
            .with("active.lock", &lock(42, "host-a"))
            .with("runs/r1/outcome.json", OUTCOME)
            .with("runs/r1/terminal-claim.json", r#"{"claimantPid":77}"#);
        probe.alive = vec![77];
        assert_eq!(
            classify_reflection_lock_holder(Path::new("/refl"), &probe),
            ReflectionLockVerdict::Live {
                run_id: "r1".to_owned()
            }
        );
    }

    #[test]
    fn a_dead_publish_claimant_allows_dead() {
        let probe = Fake::on_this_host()
            .with("active.lock", &lock(42, "host-a"))
            .with("runs/r1/outcome.json", OUTCOME)
            .with("runs/r1/terminal-claim.json", r#"{"claimantPid":77}"#);
        assert_eq!(
            classify_reflection_lock_holder(Path::new("/refl"), &probe),
            ReflectionLockVerdict::Dead {
                run_id: "r1".to_owned()
            }
        );
    }

    #[test]
    fn a_claim_without_a_claimant_is_stuck() {
        let probe = Fake::on_this_host()
            .with("active.lock", &lock(42, "host-a"))
            .with("runs/r1/outcome.json", OUTCOME)
            .with("runs/r1/terminal-claim.json", "{}");
        assert!(matches!(
            classify_reflection_lock_holder(Path::new("/refl"), &probe),
            ReflectionLockVerdict::Stuck { .. }
        ));
    }

    #[test]
    fn a_probe_that_cannot_judge_pid_zero_yields_stuck() {
        let probe = Fake::on_this_host()
            .with("active.lock", &lock(42, "host-a"))
            .with("runs/r1/outcome.json", OUTCOME)
            .with("runs/r1/terminal-claim.json", r#"{"claimantPid":0}"#);
        let verdict = classify_reflection_lock_holder(Path::new("/refl"), &probe);
        assert!(matches!(verdict, ReflectionLockVerdict::Stuck { .. }));
    }

    #[test]
    fn an_invalid_claim_is_stuck() {
        let probe = Fake::on_this_host()
            .with("active.lock", &lock(42, "host-a"))
            .with("runs/r1/outcome.json", OUTCOME)
            .with("runs/r1/terminal-claim.json", "nope");
        assert!(matches!(
            classify_reflection_lock_holder(Path::new("/refl"), &probe),
            ReflectionLockVerdict::Stuck { .. }
        ));
    }

    #[test]
    fn an_unreadable_outcome_is_stuck() {
        let probe = Fake::on_this_host()
            .with("active.lock", &lock(42, "host-a"))
            .with("runs/r1/outcome.json", "not json");
        assert!(matches!(
            classify_reflection_lock_holder(Path::new("/refl"), &probe),
            ReflectionLockVerdict::Stuck { .. }
        ));
    }

    #[test]
    fn a_run_id_of_the_wrong_type_reads_as_unknown() {
        let probe =
            Fake::on_this_host().with("active.lock", r#"{"runId":7,"launcherHostname":"host-b"}"#);
        let verdict = classify_reflection_lock_holder(Path::new("/refl"), &probe);
        match verdict {
            ReflectionLockVerdict::Stuck { run_id, .. } => assert_eq!(run_id, "unknown"),
            other => panic!("expected stuck, got {other:?}"),
        }
    }

    #[test]
    fn an_optional_attempt_must_be_a_positive_integer() {
        let with_attempt = |attempt: &str| {
            format!(
                r#"{{"version":1,"runId":"r1","finishedAt":"t","attempt":{attempt},"childExit":{{"code":null,"signal":"SIGTERM"}},"timedOut":true}}"#
            )
        };
        let probe = Fake::on_this_host()
            .with("active.lock", &lock(42, "host-a"))
            .with("runs/r1/outcome.json", &with_attempt("2"));
        assert_eq!(
            classify_reflection_lock_holder(Path::new("/refl"), &probe),
            ReflectionLockVerdict::Dead {
                run_id: "r1".to_owned()
            }
        );
        for bad in ["0", "-1", "1.5"] {
            let probe = Fake::on_this_host()
                .with("active.lock", &lock(42, "host-a"))
                .with("runs/r1/outcome.json", &with_attempt(bad));
            assert!(
                matches!(
                    classify_reflection_lock_holder(Path::new("/refl"), &probe),
                    ReflectionLockVerdict::Stuck { .. }
                ),
                "attempt {bad} should not be a recorded outcome"
            );
        }
    }
    #[test]
    fn the_real_probe_reports_the_running_process_start_identity() {
        let identity = SystemReflectionLockProbe
            .process_start_identity(std::process::id())
            .unwrap();
        #[cfg(target_os = "linux")]
        assert!(
            identity
                .as_deref()
                .is_some_and(|value| value.starts_with("proc-start:")),
            "a live pid must yield a start identity, got {identity:?}"
        );
        #[cfg(not(target_os = "linux"))]
        assert!(identity.is_none());
    }

    #[test]
    fn the_real_probe_reports_this_machine_hostname() {
        assert_eq!(
            SystemReflectionLockProbe.hostname(),
            crate::host_identity::hostname()
        );
    }

    #[test]
    fn a_reused_pid_is_dead_when_the_recorded_start_differs() {
        let recorded = "proc-start:111";
        let actual = SystemReflectionLockProbe
            .process_start_identity(std::process::id())
            .unwrap();
        #[cfg(target_os = "linux")]
        {
            let actual = actual.expect("a live pid has a start identity");
            assert_ne!(actual, recorded);
            let liveness = launcher_liveness(
                f64::from(std::process::id()),
                Some(recorded),
                &SystemReflectionLockProbe,
            );
            assert_eq!(liveness, ProcessLiveness::Dead);
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = actual;
            assert_eq!(
                launcher_liveness(
                    f64::from(std::process::id()),
                    Some(recorded),
                    &SystemReflectionLockProbe
                ),
                ProcessLiveness::Alive
            );
        }
    }
}
