//! senpi `modes/rpc/compact-lane-current.ts`: the lane's compact current state for one epoch
//! (S03b-3), wrapping `CompactSessionState` and tracking protocol-depth refusals without
//! breaking legacy delivery.

use crate::compact_cold_flows::CompactColdFlow;
use crate::compact_cold_operations::{CompactColdOperation, CompactColdOutcome};
use crate::compact_current_components::is_component_record;
use crate::compact_lane_transitions::{CanonicalUnit, lane_transition};
use crate::compact_message_identity::LiveIdentity;
use crate::compact_protocol_depth::{
    ProtocolDepthError, assert_protocol_depth, protocol_clone, protocol_depth_limit_of,
};
use crate::compact_read_lease::{CompactError, CompactReadReason};
use crate::compact_session_state::{CompactSessionState, CompactSessionStateOptions};
use crate::compact_state_view::{CompactStateView, FrozenCompactState};
use crate::compact_transcript_flows::{TranscriptFlow, compact_cold_flow};
use crate::compact_transition::{CompactPersistence, CompactTransition};
use futures::future::BoxFuture;
use maho_core::session_transcript_journal::TranscriptTransition;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

/// Execution/hook/boundary call records (from `compact-call-model.ts`).
pub const CALL_RECORDS: [&str; 6] = [
    "tool_execution_start",
    "tool_execution_update",
    "tool_execution_end",
    "hook_execution_start",
    "hook_execution_end",
    "turn_end",
];

pub fn is_call_record(record_type: &str) -> bool {
    CALL_RECORDS.contains(&record_type)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallFold {
    pub changed: bool,
    pub identity: Option<LiveIdentity>,
    pub projection: Option<Value>,
}

pub type MessagesLookupFn =
    Arc<dyn Fn(&[String]) -> BoxFuture<'_, Vec<Option<String>>> + Send + Sync>;

pub struct CompactLaneCurrent {
    state: Option<Arc<CompactSessionState>>,
    cleanup: Vec<tokio::task::JoinHandle<()>>,
    cold_chain: Option<BoxFuture<'static, ()>>,
    owners: HashMap<u64, Arc<CompactSessionState>>,
    next_cut_id: u64,
    cuts: HashMap<u64, FrozenCompactState>,
    refused: u64,
}

impl Default for CompactLaneCurrent {
    fn default() -> Self {
        Self::new()
    }
}

impl CompactLaneCurrent {
    pub fn new() -> Self {
        Self {
            state: None,
            cleanup: Vec::new(),
            cold_chain: None,
            owners: HashMap::new(),
            next_cut_id: 1,
            cuts: HashMap::new(),
            refused: 0,
        }
    }

    pub fn has_state(&self) -> bool {
        self.state.is_some()
    }

    /// Records refused at the protocol depth limit over this lane's lifetime.
    pub fn refused_records(&self) -> u64 {
        self.refused
    }

    pub fn bootstrap(&mut self, options: CompactSessionStateOptions, _epoch: &str) {
        self.retire();
        self.state = Some(Arc::new(CompactSessionState::new(options)));
    }

    /// The one protocol depth check of a legacy record a compact fold would clone or serialize
    /// (RULING-59 C). A refused record is counted once; a live state stays available.
    pub fn admits(&mut self, record: &Value, record_type: &str) -> bool {
        if record_type != "message_end"
            && !is_call_record(record_type)
            && !is_component_record(record_type)
        {
            return true;
        }
        match assert_protocol_depth(record) {
            Ok(()) => true,
            Err(err) => {
                if let Some(limit) = protocol_depth_limit_of(&err) {
                    self.refuse(limit);
                } else {
                    self.fail_reason(&err.to_string());
                }
                false
            }
        }
    }

    pub fn fold(&mut self, unit: &CanonicalUnit, _sequence: u64, _turn_id: Option<&str>) {
        let Some(ref state) = self.state else {
            return;
        };
        let Some(transition) = lane_transition(unit) else {
            return;
        };
        // Current Rust `CompactSessionState` accepts `CompactTransition` (S03b-1); seeded transitions
        // validate protocol depth and prepare for S03b-2b integration.
        let _ = (state, transition);
    }

    pub fn accept_transition(&mut self, transition: &CompactTransition) {
        let Some(ref state) = self.state else {
            return;
        };
        if let Err(err) = state.accept(transition) {
            self.fail_reason(&err.to_string());
        }
    }

    pub fn fold_component(&mut self, record: &Value, _sequence: u64) {
        if self.state.is_none() {
            return;
        }
        if let Err(err) = protocol_clone(record) {
            self.fail_depth(err);
        }
    }

    pub fn fold_call(
        &mut self,
        record: &Value,
        _sequence: u64,
        _turn_id: Option<&str>,
        _call_key: Option<&str>,
    ) -> Option<CallFold> {
        let _state = self.state.as_ref()?;
        match protocol_clone(record) {
            Ok(_) => None,
            Err(err) => {
                self.fail_depth(err);
                None
            }
        }
    }

    pub fn transcript(
        &mut self,
        transition: &TranscriptTransition,
        flow: Option<&TranscriptFlow>,
        messages_of: Option<MessagesLookupFn>,
        epoch: &str,
    ) {
        if let TranscriptTransition::Append {
            owning_message_id: Some(message_id),
            recovery_epoch: Some(rec_epoch),
            ..
        } = transition
            && rec_epoch == epoch
        {
            self.status(message_id, CompactPersistence::Appended);
        }
        if let (Some(flow), Some(lookup), Some(state)) = (flow, messages_of, self.state.clone()) {
            let flow = flow.clone();
            let fut = async move {
                let Some(cold) = compact_cold_flow(&flow, &|ids| lookup(ids)).await else {
                    return;
                };
                match cold {
                    CompactColdFlow::Compaction { removed } => {
                        for id in removed {
                            let _ = state
                                .apply_cold(CompactColdOperation::MessageRemove { message_id: id })
                                .await;
                        }
                    }
                    CompactColdFlow::ContextEditRemoval { message_id } => {
                        let _ = state
                            .apply_cold(CompactColdOperation::MessageRemove { message_id })
                            .await;
                    }
                    CompactColdFlow::Branch { removed, .. } => {
                        for id in removed {
                            let _ = state
                                .apply_cold(CompactColdOperation::MessageRemove { message_id: id })
                                .await;
                        }
                    }
                    CompactColdFlow::Edit { .. } => {}
                }
            };
            self.enqueue_cold(fut);
        }
    }

    pub fn status(&mut self, message_id: &str, persistence: CompactPersistence) {
        let Some(state) = self.state.clone() else {
            return;
        };
        let msg_id = message_id.to_owned();
        self.enqueue_cold(async move {
            let outcome = state
                .apply_cold(CompactColdOperation::MessageStatus {
                    message_id: msg_id,
                    persistence,
                })
                .await;
            if let CompactColdOutcome::Failed { cause, .. } = outcome {
                eprintln!("senpi rpc acceptance: cold status failed: {cause}");
            }
        });
    }

    fn enqueue_cold(&mut self, fut: impl std::future::Future<Output = ()> + Send + 'static) {
        let prev = self.cold_chain.take();
        self.cold_chain = Some(Box::pin(async move {
            if let Some(p) = prev {
                p.await;
            }
            fut.await;
        }));
    }

    pub fn freeze(&mut self) -> Option<u64> {
        let state = self.state.clone()?;
        match state.freeze() {
            Ok(cut) => {
                let id = self.next_cut_id;
                self.next_cut_id += 1;
                self.owners.insert(id, state);
                self.cuts.insert(id, cut);
                Some(id)
            }
            Err(err) => {
                self.fail_reason(&err.to_string());
                None
            }
        }
    }

    pub fn release(&mut self, cut_id: Option<u64>) {
        let Some(id) = cut_id else {
            return;
        };
        if let (Some(owner), Some(cut)) = (self.owners.remove(&id), self.cuts.remove(&id)) {
            let _ = owner.release(&cut);
        }
    }

    pub async fn read(&self, cut_id: Option<u64>) -> Result<CompactStateView, CompactError> {
        let Some(id) = cut_id else {
            return Err(CompactError::Io("unavailable".into()));
        };
        let (Some(owner), Some(cut)) = (self.owners.get(&id), self.cuts.get(&id)) else {
            return Err(CompactError::read(CompactReadReason::Ownership));
        };
        owner.read(cut).await
    }

    pub fn retire(&mut self) {
        let Some(state) = self.state.take() else {
            return;
        };
        if tokio::runtime::Handle::try_current().is_ok() {
            self.cleanup.push(tokio::spawn(async move {
                if let Err(err) = state.retire().await {
                    eprintln!("senpi rpc acceptance: compact current state retire failed: {err}");
                }
            }));
        }
    }

    pub async fn idle(&mut self) {
        for handle in self.cleanup.drain(..) {
            let _ = handle.await;
        }
        if let Some(chain) = self.cold_chain.take() {
            chain.await;
        }
        if let Some(ref state) = self.state {
            state.idle().await;
        }
    }

    pub fn fail_depth(&mut self, err: ProtocolDepthError) {
        if let Some(limit) = protocol_depth_limit_of(&err) {
            self.refuse(limit);
        } else {
            self.fail_reason(&err.to_string());
        }
    }

    pub fn fail_reason(&mut self, reason: &str) {
        if self.state.is_none() {
            return;
        }
        eprintln!("senpi rpc acceptance: compact current state unavailable: {reason}");
        self.retire();
    }

    fn refuse(&mut self, limit: usize) {
        self.refused += 1;
        eprintln!(
            "senpi rpc acceptance: compact record refused: unsupported_value (depth above {limit})"
        );
    }
}

/// Shared wrapper for concurrent lane callers.
pub type SharedCompactLaneCurrent = Arc<Mutex<CompactLaneCurrent>>;

pub fn lock_lane_current(
    lane: &SharedCompactLaneCurrent,
) -> std::sync::MutexGuard<'_, CompactLaneCurrent> {
    lane.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact_content_store::CompactContentStoreOptions;
    use crate::compact_protocol_depth::COMPACT_PROTOCOL_MAX_DEPTH;
    use serde_json::json;

    #[tokio::test]
    async fn admits_counts_depth_refusals_without_retiring_live_state() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut current = CompactLaneCurrent::new();
        current.bootstrap(
            CompactSessionStateOptions {
                storage: CompactContentStoreOptions {
                    dir: dir.path().join("curr").display().to_string(),
                    ..Default::default()
                },
                compact_above_rows: None,
            },
            "e1",
        );
        assert!(current.has_state());

        let mut deep = json!(1);
        for _ in 0..=COMPACT_PROTOCOL_MAX_DEPTH {
            deep = Value::Array(vec![deep]);
        }
        assert!(!current.admits(&deep, "message_end"));
        assert_eq!(current.refused_records(), 1);
        assert!(current.has_state());

        current.fail_reason("storage_unavailable");
        assert!(!current.has_state());
        current.idle().await;
    }

    #[tokio::test]
    async fn freezes_reads_and_releases_current_state() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut current = CompactLaneCurrent::new();
        current.bootstrap(
            CompactSessionStateOptions {
                storage: CompactContentStoreOptions {
                    dir: dir.path().join("curr2").display().to_string(),
                    ..Default::default()
                },
                compact_above_rows: None,
            },
            "e1",
        );
        let cut = current.freeze().expect("freeze");
        let view = current.read(Some(cut)).await.expect("read");
        assert_eq!(view.sequence, 0);
        current.release(Some(cut));
        assert!(current.read(Some(cut)).await.is_err());
        current.retire();
        current.idle().await;
    }
}
