//! senpi `modes/rpc/session-acceptance-lane.ts`: the ordered acceptance lane of one routing session.
//! Every record reaches it synchronously, before `SessionEventWriter.enqueue`, in the order the session
//! produced it; a worker's records arrive here before the worker's credit returns. Each bootstrap
//! starts a new `session_epoch` whose sequences are never compared with another epoch's.
//!
//! PERF-DIVERGENCE(senpi settled retirement): upstream retires the previous epoch's settled store with
//! an unawaited promise chain; here the retirement is spawned on the current tokio runtime and joined
//! by `idle()`.
use crate::accepted_transcript::{AcceptedTranscript, TranscriptRoot};
use crate::client_message_events::correlated_record;
use crate::compact_content_store::CompactContentStoreOptions;
use crate::compact_settled_messages::{
    CompactSettledMessages, SettledDiagnostic, SettledRead, SettledRoot,
};
use maho_core::session_recovery_identity::RecoveryCarrier;
use maho_core::session_transcript_journal::TranscriptTransition;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

/// Where one accepted recovery record sits in its runtime incarnation (R3.C1.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedRef {
    pub session_epoch: String,
    pub sequence: u64,
    pub history_version: u64,
    pub message_id: Option<String>,
    pub turn_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AcceptedCorrelation {
    pub sequence: u64,
    pub client_message_id: Option<Value>,
    pub client_turn_id: Option<Value>,
    pub client_messages: Option<Value>,
}

#[derive(Clone)]
pub struct FrozenAcceptedRoot {
    pub session_epoch: String,
    pub sequence: u64,
    pub active_message_id: Option<String>,
    pub turn_id: Option<String>,
    pub correlation: Option<AcceptedCorrelation>,
    pub transcript: TranscriptRoot,
    /// `None` for a retired lane: its paged settled store refuses new cuts.
    pub settled: Option<SettledRoot>,
    id: u64,
}

/// The incarnation's transcript as its manager journaled it up to bootstrap.
///
/// Upstream's `epochSignal` SharedArrayBuffer (the worker replacement handshake) is the worker lane's
/// concern; the Rust worker protocol carries the nonce in its frame instead (see the session-31 row).
#[derive(Debug, Clone, Default)]
pub struct SessionBootstrap {
    pub transcript: Vec<TranscriptTransition>,
}

/// Recovery kinds take a sequence; responses and every other record keep FIFO order with none.
const SEQUENCED: [&str; 13] = [
    "agent_start",
    "agent_end",
    "turn_start",
    "turn_end",
    "message_start",
    "message_update",
    "message_end",
    "tool_execution_start",
    "tool_execution_update",
    "tool_execution_end",
    "entry_appended",
    "session_abort",
    "transcript_write_failed",
];

fn turn_scoped(kind: &str, record: &Value) -> bool {
    if kind == "turn_end" || kind.starts_with("tool_execution_") {
        return true;
    }
    if !kind.starts_with("message_") {
        return false;
    }
    matches!(
        record["message"]["role"].as_str(),
        Some("assistant" | "toolResult")
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetireReason {
    Close,
    WorkerFailed,
}

type Diagnose = Arc<dyn Fn(SettledDiagnostic) + Send + Sync>;

pub struct SessionAcceptanceLane {
    epoch: String,
    sequence: u64,
    transcript: AcceptedTranscript,
    active_message_id: Option<String>,
    turn_id: Option<String>,
    correlation: Option<AcceptedCorrelation>,
    live: HashMap<u64, Arc<CompactSettledMessages>>,
    next_root: u64,
    retired_as: Option<RetireReason>,
    nonce: String,
    settled: Option<Arc<CompactSettledMessages>>,
    cleanup: Vec<tokio::task::JoinHandle<()>>,
    storage: Option<CompactContentStoreOptions>,
    diagnose: Diagnose,
}

fn uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}

impl Default for SessionAcceptanceLane {
    fn default() -> Self {
        Self::new(None, None)
    }
}

impl SessionAcceptanceLane {
    pub fn new(storage: Option<CompactContentStoreOptions>, diagnose: Option<Diagnose>) -> Self {
        Self {
            epoch: String::new(),
            sequence: 0,
            transcript: AcceptedTranscript::default(),
            active_message_id: None,
            turn_id: None,
            correlation: None,
            live: HashMap::new(),
            next_root: 1,
            retired_as: None,
            nonce: uuid(),
            settled: None,
            cleanup: Vec::new(),
            storage,
            diagnose: diagnose.unwrap_or_else(|| {
                Arc::new(|reason: SettledDiagnostic| {
                    eprintln!(
                        "senpi rpc acceptance: settled transition: {}",
                        reason.as_str()
                    )
                })
            }),
        }
    }

    pub fn recovery_epoch(&self) -> &str {
        &self.nonce
    }

    pub fn session_epoch(&self) -> &str {
        &self.epoch
    }

    pub fn retired(&self) -> bool {
        self.retired_as.is_some()
    }

    pub fn live_roots(&self) -> usize {
        self.live.len()
    }

    pub fn bootstrap(&mut self, input: SessionBootstrap, nonce: Option<String>) {
        if self.retired() {
            return;
        }
        if self.settled.is_some() {
            self.retire_settled();
        }
        self.nonce = nonce.unwrap_or_else(uuid);
        let mut storage = self.storage.clone().unwrap_or_default();
        let parent = if storage.dir.is_empty() {
            std::env::temp_dir()
        } else {
            std::path::PathBuf::from(&storage.dir)
        };
        storage.dir = parent
            .join(format!("senpi-settled-{}", uuid()))
            .display()
            .to_string();
        let diagnose = self.diagnose.clone();
        self.settled = Some(Arc::new(CompactSettledMessages::new(
            self.nonce.clone(),
            storage,
            move |reason| diagnose(reason),
        )));
        self.epoch = uuid();
        self.sequence = 0;
        self.transcript = AcceptedTranscript::default();
        for transition in &input.transcript {
            self.transcript.apply(transition);
        }
        self.active_message_id = None;
        self.turn_id = None;
        self.correlation = None;
    }

    /// Transcript transitions committed before the record they arrive with; each takes its own sequence (C1.1).
    pub fn apply_transcript(&mut self, transitions: &[TranscriptTransition]) {
        if self.retired() || self.epoch.is_empty() {
            return;
        }
        for transition in transitions {
            self.sequence += 1;
            self.transcript.apply(transition);
            if matches!(transition, TranscriptTransition::Append { .. })
                && self.transcript.available()
                && let Some(settled) = &self.settled
                && let Err(error) = settled.append(transition)
            {
                eprintln!("senpi rpc acceptance: settled append failed: {error}");
            }
        }
    }

    pub fn accept(
        &mut self,
        record: &Value,
        carrier: Option<&RecoveryCarrier>,
    ) -> Option<AcceptedRef> {
        let kind = record.get("type").and_then(Value::as_str)?;
        if self.retired() || self.epoch.is_empty() || !SEQUENCED.contains(&kind) {
            return None;
        }
        let settled = self.settled.clone()?;
        self.accept_correlation(record);
        self.sequence += 1;
        if kind == "turn_start" || (self.turn_id.is_none() && turn_scoped(kind, record)) {
            self.turn_id = Some(uuid());
        }
        let turn_id = self.turn_id.clone();
        if kind == "turn_end" || kind == "agent_end" {
            self.turn_id = None;
        }
        let message_id = if kind.starts_with("message_") {
            settled
                .valid(carrier)
                .map(|carrier| carrier.message_id.clone())
        } else {
            None
        };
        if kind == "message_start" {
            self.active_message_id = message_id.clone();
        }
        let stored = match kind {
            "message_end" => settled.end(record, carrier, self.sequence, turn_id.clone()),
            "transcript_write_failed" => {
                let error = match record.get("errorMessage") {
                    Some(Value::String(text)) => text.clone(),
                    Some(other) => other.to_string(),
                    None => "undefined".into(),
                };
                settled.fail(carrier, &error)
            }
            _ => Ok(()),
        };
        if let Err(error) = stored {
            eprintln!("senpi rpc acceptance: settled transition failed: {error}");
        }
        if kind == "message_end" && self.active_message_id == message_id {
            self.active_message_id = None;
        }
        Some(AcceptedRef {
            session_epoch: self.epoch.clone(),
            sequence: self.sequence,
            history_version: self.transcript.history_version(),
            message_id,
            turn_id,
        })
    }

    pub fn freeze(&mut self) -> FrozenAcceptedRoot {
        let id = self.next_root;
        self.next_root += 1;
        let settled = match (&self.settled, self.retired()) {
            (Some(owner), false) => owner.freeze().ok().map(|root| (owner.clone(), root)),
            _ => None,
        };
        let root = FrozenAcceptedRoot {
            session_epoch: self.epoch.clone(),
            sequence: self.sequence,
            active_message_id: self.active_message_id.clone(),
            turn_id: self.turn_id.clone(),
            correlation: self.correlation.clone(),
            transcript: self.transcript.freeze(),
            settled: settled.as_ref().map(|(_, root)| root.clone()),
            id,
        };
        if let Some((owner, _)) = settled {
            self.live.insert(id, owner);
        }
        root
    }

    pub fn release(&mut self, root: &FrozenAcceptedRoot) {
        if let Some(owner) = self.live.remove(&root.id)
            && let Some(settled) = &root.settled
        {
            owner.release(settled);
        }
    }

    pub async fn read_settled(&self, root: &FrozenAcceptedRoot) -> Result<SettledRead, String> {
        let owner = self.live.get(&root.id).filter(|_| !self.retired());
        let (Some(owner), Some(settled)) = (owner, &root.settled) else {
            return Err("accepted root released".into());
        };
        owner.read(settled).await.map_err(|error| error.to_string())
    }

    pub fn storage_status(
        &self,
    ) -> Option<(
        crate::compact_content_store::CompactStorageStatus,
        crate::compact_content_index::IndexStatus,
    )> {
        self.settled
            .as_ref()
            .map(|settled| settled.storage_status())
    }

    pub async fn idle(&mut self) {
        for handle in self.cleanup.drain(..) {
            let _ = handle.await;
        }
        if let Some(settled) = &self.settled {
            settled.idle().await;
        }
    }

    fn retire_settled(&mut self) {
        for (_, owner) in std::mem::take(&mut self.live) {
            // Releasing the owner's cuts happens through `release`; dropped roots retire with the store.
            drop(owner);
        }
        let Some(settled) = self.settled.clone() else {
            return;
        };
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        let diagnose = self.diagnose.clone();
        self.cleanup.push(tokio::spawn(async move {
            if settled.retire().await.is_err() {
                diagnose(SettledDiagnostic::RetiredOwner);
            }
        }));
    }

    /// No record is accepted after retirement, and every root this lane published is released.
    pub fn retire(&mut self, reason: RetireReason) {
        self.retired_as.get_or_insert(reason);
        if self.settled.is_some() {
            self.retire_settled();
        }
    }

    /// A change of the outer client correlation is its own transition, sequenced just before its record (C1.1).
    fn accept_correlation(&mut self, record: &Value) {
        // Within a turn, any identity-less correlated record other than turn_start keeps the standing
        // correlation and gains no client identity of its own; only a new turn boundary ends it (ruling B).
        let identified =
            record.get("clientMessageId").is_some() || record.get("clientTurnId").is_some();
        if !identified && (self.correlation.is_none() || !correlated_record(record)) {
            return;
        }
        if !identified
            && self.turn_id.is_some()
            && record.get("type").and_then(Value::as_str) != Some("turn_start")
        {
            return;
        }
        let next = (
            record.get("clientMessageId").cloned(),
            record.get("clientTurnId").cloned(),
            record.get("clientMessages").cloned(),
        );
        if let Some(current) = &self.correlation {
            let same_messages = match (&current.client_messages, &next.2) {
                (None, None) => true,
                (Some(Value::Array(a)), Some(Value::Array(b))) => {
                    a.len() == b.len()
                        && a.iter().zip(b).all(|(x, y)| {
                            x.get("clientMessageId") == y.get("clientMessageId")
                                && x.get("clientTurnId") == y.get("clientTurnId")
                        })
                }
                (a, b) => a == b && !matches!(a, Some(Value::Array(_) | Value::Object(_))),
            };
            if current.client_message_id == next.0
                && current.client_turn_id == next.1
                && same_messages
            {
                return;
            }
        }
        self.sequence += 1;
        self.correlation = Some(AcceptedCorrelation {
            sequence: self.sequence,
            client_message_id: next.0,
            client_turn_id: next.1,
            client_messages: next.2,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn lane(root: &tempfile::TempDir) -> SessionAcceptanceLane {
        let mut lane = SessionAcceptanceLane::new(
            Some(CompactContentStoreOptions {
                dir: root.path().display().to_string(),
                ..Default::default()
            }),
            None,
        );
        lane.bootstrap(SessionBootstrap::default(), Some("nonce-1".into()));
        lane
    }

    fn carrier(id: &str, epoch: &str) -> RecoveryCarrier {
        RecoveryCarrier {
            message_id: id.into(),
            epoch: epoch.into(),
            durable_entry_id: None,
        }
    }

    // senpi 4272-inprocess-acceptance / epoch-per-replacement: recovery kinds sequence, others do not.
    #[tokio::test]
    async fn sequences_recovery_kinds_and_restarts_per_bootstrap() {
        let root = tempfile::tempdir().expect("tempdir");
        let mut lane = lane(&root);
        assert_eq!(lane.recovery_epoch(), "nonce-1");
        assert!(lane.accept(&json!({"type":"response"}), None).is_none());
        let start = lane
            .accept(
                &json!({"type":"message_start","message":{"role":"assistant"}}),
                Some(&carrier("m", "nonce-1")),
            )
            .expect("accepted");
        assert_eq!(start.sequence, 1);
        assert_eq!(start.message_id.as_deref(), Some("m"));
        assert!(start.turn_id.is_some());
        let foreign = lane
            .accept(
                &json!({"type":"message_update","message":{"role":"assistant"}}),
                Some(&carrier("m", "other")),
            )
            .expect("accepted");
        assert_eq!(foreign.message_id, None);
        lane.apply_transcript(&[TranscriptTransition::Leaf { leaf_id: None }]);
        let end = lane
            .accept(
                &json!({"type":"message_end","message":{"role":"assistant","content":"x"}}),
                Some(&carrier("m", "nonce-1")),
            )
            .expect("accepted");
        assert_eq!((end.sequence, end.history_version), (4, 1));
        let mut root_cut = lane.freeze();
        assert_eq!(root_cut.active_message_id, None);
        let settled = lane.read_settled(&root_cut).await.expect("settled");
        assert_eq!(settled.settled_messages.len(), 1);
        lane.release(&root_cut);
        assert!(lane.read_settled(&root_cut).await.is_err());
        let epoch = lane.session_epoch().to_owned();
        lane.bootstrap(SessionBootstrap::default(), None);
        assert_ne!(lane.session_epoch(), epoch);
        assert_ne!(lane.recovery_epoch(), "nonce-1");
        let first = lane
            .accept(&json!({"type":"agent_start"}), None)
            .expect("accepted");
        assert_eq!(first.sequence, 1);
        lane.retire(RetireReason::Close);
        assert!(lane.accept(&json!({"type":"agent_end"}), None).is_none());
        root_cut = lane.freeze();
        assert!(root_cut.settled.is_none());
        assert_eq!(lane.live_roots(), 0);
        lane.idle().await;
    }

    // senpi 4272-midturn-correlation: identity-less input keeps the standing correlation within a turn.
    #[tokio::test]
    async fn keeps_the_standing_correlation_within_a_turn() {
        let root = tempfile::tempdir().expect("tempdir");
        let mut lane = lane(&root);
        lane.accept(
            &json!({"type":"turn_start","clientMessageId":"c1","clientTurnId":"t1"}),
            None,
        );
        let cut = lane.freeze();
        let correlation = cut.correlation.clone().expect("correlation");
        assert_eq!(correlation.client_message_id, Some(json!("c1")));
        assert_eq!(correlation.sequence, 1);
        lane.release(&cut);
        lane.accept(
            &json!({"type":"message_start","message":{"role":"user"}}),
            None,
        );
        let cut = lane.freeze();
        assert_eq!(cut.correlation.expect("kept").sequence, 1);
        assert_eq!(cut.sequence, 3);
        lane.retire(RetireReason::Close);
        lane.idle().await;
    }
}
