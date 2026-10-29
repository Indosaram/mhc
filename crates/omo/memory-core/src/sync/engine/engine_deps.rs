use std::rc::Rc;

use super::error::EngineResult;

pub type SyncLog = Rc<dyn Fn(&str)>;
use super::ports::{Clock, ItemStore, Sealer, SyncRepo, Transport, Verifier, WriterLock};

/// Where a crash can be injected: right after each journal marker, or between an effect and its marker.
pub const INBOUND_POINTS: [&str; 12] = [
    "captured",
    "persisted",
    "downloaded",
    "verified",
    "merged",
    "staged",
    "render-intent",
    "render-key",
    "rendered",
    "git-commit",
    "committed",
    "checkpointed",
];
pub const OUTBOUND_POINTS: [&str; 5] = [
    "sealed",
    "reserved",
    "uploaded",
    "append-landed",
    "appended",
];

pub type EnginePoint = &'static str;
/// A fault hook; returning an error (usually [`CrashInjected`]) aborts the cycle at that point.
pub type EngineFault = Rc<dyn Fn(EnginePoint) -> EngineResult<()>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrashInjected {
    pub point: String,
}

impl CrashInjected {
    pub fn new(point: &str) -> Self {
        Self {
            point: point.to_string(),
        }
    }
}

impl std::fmt::Display for CrashInjected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "crash injected at {}", self.point)
    }
}
impl std::error::Error for CrashInjected {}

pub struct EngineDeps {
    pub on_status_change: Option<Rc<dyn Fn()>>,
    pub repo: Rc<dyn SyncRepo>,
    pub store: Rc<dyn ItemStore>,
    pub transport: Rc<dyn Transport>,
    pub verifier: Rc<dyn Verifier>,
    pub sealer: Rc<dyn Sealer>,
    pub lock: Rc<dyn WriterLock>,
    pub clock: Rc<dyn Clock>,
    pub fault: Option<EngineFault>,
    pub log: Option<SyncLog>,
    /// The plaintext bound per pushed batch; defaults to `MAX_BATCH_BYTES`.
    pub batch_bytes: Option<usize>,
    /// Uniform `[0, 1)` source for the jittered stale-base backoff.
    pub random: Option<Rc<dyn Fn() -> f64>>,
}

impl EngineDeps {
    pub fn fault_at(&self, point: EnginePoint) -> EngineResult<()> {
        match &self.fault {
            Some(fault) => fault(point),
            None => Ok(()),
        }
    }

    pub fn log_line(&self, line: &str) {
        if let Some(log) = &self.log {
            log(line);
        }
    }
}
