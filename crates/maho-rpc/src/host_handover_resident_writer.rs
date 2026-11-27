//! The process that holds a retained session resident (senpi#6190). A host records each retained
//! session it has open, stamped with this writer, so the next host on the endpoint can restore it
//! after a crash; while that writer is still alive (a generation handoff's draining predecessor) the
//! session is not its successor's to reopen - the predecessor parks it, and the unstamped park is
//! what the successor consumes. Liveness follows the session writer claims: same host, same boot,
//! live pid whose start time matches.

use maho_core::session_writer_claims::WriterProbes;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;

pub const BOOT_INSTANT_TOLERANCE_MS: u64 = 120_000;

pub fn same_boot(left: u64, right: u64) -> bool {
    left.abs_diff(right) <= BOOT_INSTANT_TOLERANCE_MS
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResidentWriter {
    pub host: String,
    pub pid: u32,
    pub boot_at_ms: u64,
    pub process_started_at_ms: u64,
}

pub fn current_resident_writer(probes: &dyn WriterProbes) -> ResidentWriter {
    let (pid, process_started_at_ms) = probes.own();
    ResidentWriter {
        host: probes.host(),
        pid,
        boot_at_ms: probes.boot_at_ms(),
        process_started_at_ms,
    }
}

pub fn is_resident_writer(value: &Value) -> bool {
    let Some(record) = value.as_object() else {
        return false;
    };
    record.get("host").is_some_and(Value::is_string)
        && record
            .get("pid")
            .and_then(Value::as_u64)
            .is_some_and(|p| u32::try_from(p).is_ok())
        && record.get("bootAtMs").is_some_and(Value::is_number)
        && record
            .get("processStartedAtMs")
            .is_some_and(Value::is_number)
}

/// A liveness probe for one reopen watch, which polls: a writer found gone stays gone, and a live
/// pid's start time is read once. A writer on another host cannot be probed from here, so it is
/// never displaced.
pub struct ResidentWriterProbe<'a> {
    probes: &'a dyn WriterProbes,
    gone: HashSet<String>,
    verified: HashSet<String>,
}

impl<'a> ResidentWriterProbe<'a> {
    pub fn new(probes: &'a dyn WriterProbes) -> Self {
        Self {
            probes,
            gone: HashSet::new(),
            verified: HashSet::new(),
        }
    }

    pub async fn is_alive(&mut self, writer: &ResidentWriter) -> bool {
        let key = format!(
            "{}:{}:{}",
            writer.host, writer.pid, writer.process_started_at_ms
        );
        if self.gone.contains(&key) {
            return false;
        }
        if writer.host != self.probes.host() {
            return true;
        }
        if !same_boot(self.probes.boot_at_ms(), writer.boot_at_ms)
            || !self.probes.pid_alive(writer.pid)
        {
            self.gone.insert(key);
            return false;
        }
        if self.verified.contains(&key) {
            return true;
        }
        if let Some(started_at_ms) = self.probes.read_process_start_ms(writer.pid).await
            && started_at_ms.abs_diff(writer.process_started_at_ms) > BOOT_INSTANT_TOLERANCE_MS
        {
            self.gone.insert(key);
            return false;
        }
        self.verified.insert(key);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    struct MockProbes {
        host: String,
        boot: u64,
        own: (u32, u64),
        alive: AtomicBool,
        start_reads: AtomicUsize,
        observed_start: Option<u64>,
    }

    #[async_trait::async_trait]
    impl WriterProbes for MockProbes {
        fn pid_alive(&self, _pid: u32) -> bool {
            self.alive.load(Ordering::SeqCst)
        }

        async fn read_process_start_ms(&self, _pid: u32) -> Option<u64> {
            self.start_reads.fetch_add(1, Ordering::SeqCst);
            self.observed_start
        }

        fn boot_at_ms(&self) -> u64 {
            self.boot
        }

        fn host(&self) -> String {
            self.host.clone()
        }

        fn own(&self) -> (u32, u64) {
            self.own
        }
    }

    #[test]
    fn shape_validation_and_current_writer_match_schema() {
        let probes = MockProbes {
            host: "box-a".into(),
            boot: 1_000_000,
            own: (42, 1_005_000),
            alive: AtomicBool::new(true),
            start_reads: AtomicUsize::new(0),
            observed_start: Some(1_005_000),
        };
        let writer = current_resident_writer(&probes);
        let wire = serde_json::to_value(&writer).expect("json");
        assert!(is_resident_writer(&wire));
        assert!(!is_resident_writer(&json!([])));
        assert!(!is_resident_writer(&json!({"host": "a", "pid": 1.5})));
    }

    #[tokio::test]
    async fn probe_caches_verification_remembers_gone_and_keeps_remote_writers() {
        let probes = MockProbes {
            host: "box-a".into(),
            boot: 1_000_000,
            own: (42, 1_005_000),
            alive: AtomicBool::new(true),
            start_reads: AtomicUsize::new(0),
            observed_start: Some(1_005_000),
        };
        let mut probe = ResidentWriterProbe::new(&probes);
        let local = current_resident_writer(&probes);
        assert!(probe.is_alive(&local).await);
        assert!(probe.is_alive(&local).await);
        assert_eq!(probes.start_reads.load(Ordering::SeqCst), 1);

        // Once dead, stays gone even if pid_alive flips back.
        probes.alive.store(false, Ordering::SeqCst);
        assert!(!probe.is_alive(&local).await);
        probes.alive.store(true, Ordering::SeqCst);
        assert!(!probe.is_alive(&local).await);

        let remote = ResidentWriter {
            host: "box-b".into(),
            ..local
        };
        assert!(probe.is_alive(&remote).await);
    }

    #[tokio::test]
    async fn recycled_pid_with_distant_start_time_is_gone() {
        let probes = MockProbes {
            host: "box-a".into(),
            boot: 1_000_000,
            own: (42, 1_005_000),
            alive: AtomicBool::new(true),
            start_reads: AtomicUsize::new(0),
            observed_start: Some(1_500_000),
        };
        let mut probe = ResidentWriterProbe::new(&probes);
        let stale = current_resident_writer(&probes);
        assert!(!probe.is_alive(&stale).await);
    }
}
