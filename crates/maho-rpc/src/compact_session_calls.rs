//! senpi `modes/rpc/compact-session-calls.ts`: the outstanding-call phase machine (R3.C3.1-C3.3,
//! RULING-13): one reducer, phases only forward, records in any order, a key fixed at creation.

use crate::compact_call_model::{
    CallCorrelation, CallInput, CallJoin, CallOutcome, CallOutcomeSource, CallPhase,
    CallProjection, CallRaw, CallScope, FrozenCalls, OutstandingCall, TERMINAL_PHASE, fresh_call,
    runtime_item_key, terminal,
};
use crate::compact_call_retirement::{not_executed_patch, orphaned, settle_call, settled_patch};
use crate::compact_call_table::{CallSlotField, CompactCallTable};
use crate::compact_content_store::CompactContentStore;
use crate::compact_read_lease::CompactError;
use crate::compact_values::ValueRef;
use futures::future::BoxFuture;
use maho_core::session_call_keys::block_call_key;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallStep {
    pub sequence: u64,
    pub scope: CallScope,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AnnouncedBlock {
    pub message_id: String,
    pub content_index: usize,
    pub id: String,
    pub name: String,
    pub arguments: Value,
    pub resolved_name: Option<String>,
    pub correlation: Option<CallCorrelation>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExecutionStart {
    pub tool_call_id: String,
    pub tool_name: String,
    pub args: Value,
    pub parent_tool_call_id: Option<String>,
    /// The key its producer minted with a bridge-started call's start (RULING-127 (b)).
    pub key: Option<String>,
}

fn awaiting_start(call: &OutstandingCall) -> bool {
    call.start_sequence.is_none() && !terminal(call)
}

fn executing(call: &OutstandingCall) -> bool {
    call.phase == CallPhase::Executing
}

fn still_open(call: &OutstandingCall) -> bool {
    !terminal(call)
}

fn awaiting_carrier(call: &OutstandingCall) -> bool {
    call.join == CallJoin::Pending
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// The outstanding-call phase machine (R3.C3.1-C3.3, RULING-13).
pub struct CompactSessionCalls {
    pub table: CompactCallTable,
    pub sequence: u64,
    minted_at: Option<u64>,
    minted_k: usize,
}

impl CompactSessionCalls {
    pub fn new(store: CompactContentStore) -> Self {
        Self {
            table: CompactCallTable::new(store),
            sequence: 0,
            minted_at: None,
            minted_k: 0,
        }
    }

    pub fn get(&self, key: &str) -> Option<&OutstandingCall> {
        self.table.get(key)
    }

    /// `toolcall_start`: a bridge-started call joins by id and, like a fresh one, reads its raw
    /// prefix through the active block until its end.
    pub fn tool_start(
        &mut self,
        step: &CallStep,
        block: AnnouncedBlock,
    ) -> Result<(), CompactError> {
        let Some(turn_id) = step.scope.turn_id.as_deref() else {
            return Ok(());
        };
        let id = (!block.id.is_empty()).then_some(block.id);
        let raw = CallRaw::Active {
            message_id: block.message_id.clone(),
            content_index: block.content_index,
        };
        if let Some(mut bridge) = self
            .table
            .bridge_started(Some(turn_id), id.as_deref())
            .cloned()
        {
            bridge.owning_message_id = Some(block.message_id);
            bridge.content_index = Some(block.content_index);
            bridge.seed = block.arguments;
            bridge.raw = Some(raw);
            if let Some(correlation) = block.correlation {
                bridge.correlation = Some(correlation);
            }
            if bridge.requested_name.is_none() {
                bridge.requested_name = Some(block.name);
            }
            self.update(step, bridge)?;
            return Ok(());
        }
        if self
            .table
            .by_turn_id(Some(turn_id), id.as_deref())
            .is_some()
        {
            self.table.counters.collisions += 1;
        }
        let key = block_call_key(&block.message_id, block.content_index);
        let mut call = fresh_call(String::new(), turn_id.to_owned(), CallPhase::Streaming);
        call.tool_call_id = id;
        call.owning_message_id = Some(block.message_id);
        call.content_index = Some(block.content_index);
        call.requested_name = (!block.name.is_empty()).then_some(block.name);
        call.resolved_name = block.resolved_name;
        call.seed = block.arguments;
        call.raw = Some(raw);
        call.correlation = block.correlation;
        self.create(step, Some(key), call)?;
        Ok(())
    }

    pub fn requested_name(
        &mut self,
        step: &CallStep,
        message_id: &str,
        content_index: usize,
        name: String,
    ) -> Result<(), CompactError> {
        if let Some(mut call) = self.table.by_owner(message_id, content_index).cloned() {
            call.requested_name = Some(name);
            self.update(step, call)?;
        }
        Ok(())
    }

    /// `toolcall_end` and each terminal block: the announcement's final id, name and input; the
    /// call owns the raw.
    pub fn announce(
        &mut self,
        step: &CallStep,
        block: AnnouncedBlock,
        raw: Option<CallRaw>,
    ) -> Result<(), CompactError> {
        let turn_id = step.scope.turn_id.as_deref();
        let owned = self
            .table
            .by_owner(&block.message_id, block.content_index)
            .cloned();
        let id = (!block.id.is_empty()).then_some(block.id);
        let call = match owned {
            Some(call) => Some(call),
            None => match self.table.bridge_started(turn_id, id.as_deref()).cloned() {
                Some(mut bridge) => {
                    bridge.owning_message_id = Some(block.message_id.clone());
                    bridge.content_index = Some(block.content_index);
                    Some(self.update(step, bridge)?)
                }
                None => None,
            },
        };
        let Some(mut call) = call else {
            let Some(turn_id) = turn_id else {
                return Ok(());
            };
            let key = block_call_key(&block.message_id, block.content_index);
            let mut call = fresh_call(
                String::new(),
                turn_id.to_owned(),
                CallPhase::AwaitingExecution,
            );
            call.tool_call_id = id;
            call.owning_message_id = Some(block.message_id);
            call.content_index = Some(block.content_index);
            call.requested_name = (!block.name.is_empty()).then_some(block.name);
            call.resolved_name = block.resolved_name;
            call.raw = raw;
            call.input = CallInput {
                announced: block.arguments,
                started: Value::Null,
                prepared: Value::Null,
            };
            call.correlation = block.correlation;
            self.create(step, Some(key), call)?;
            return Ok(());
        };
        let taken = id
            .as_deref()
            .filter(|new_id| Some(*new_id) != call.tool_call_id.as_deref())
            .and_then(|new_id| self.table.indexed_to(&call.turn_id, new_id));
        if taken.is_some() {
            self.table.counters.collisions += 1;
        } else if let Some(id) = id {
            call.tool_call_id = Some(id);
        }
        if call.phase == CallPhase::Streaming {
            call.phase = CallPhase::AwaitingExecution;
        }
        if !block.name.is_empty() {
            call.requested_name = Some(block.name);
        }
        if let Some(resolved) = block.resolved_name {
            call.resolved_name = Some(resolved);
        }
        call.input.announced = block.arguments;
        if !matches!(call.raw, Some(CallRaw::Owned { .. })) {
            call.raw = raw;
        }
        self.update(step, call)?;
        Ok(())
    }

    /// Each per-id execution record returns the call it changed (RULING-123), `None` when it
    /// changed none.
    pub fn execution_start(
        &mut self,
        step: &CallStep,
        start: ExecutionStart,
    ) -> Result<Option<OutstandingCall>, CompactError> {
        let Some(turn_id) = step.scope.turn_id.as_deref() else {
            return Ok(None);
        };
        let started_at = now_ms();
        if let Some(mut announced) = self
            .table
            .oldest_by_id(&start.tool_call_id, Some(turn_id), |call| {
                call.turn_id == turn_id && awaiting_start(call)
            })
            .cloned()
        {
            announced.start_sequence = Some(step.sequence);
            announced.started_at_ms = Some(started_at);
            announced.resolved_name = Some(start.tool_name);
            announced.phase = CallPhase::Executing;
            announced.input.started = start.args;
            return self.update(step, announced).map(Some);
        }
        let existing = self
            .table
            .by_turn_id(Some(turn_id), Some(&start.tool_call_id))
            .is_some();
        if existing {
            self.table.counters.reused += 1;
        }
        let parent_key = start
            .parent_tool_call_id
            .as_deref()
            .and_then(|pid| self.lookup(Some(turn_id), pid))
            .map(|parent| parent.runtime_item_key.clone());
        let mut call = fresh_call(String::new(), turn_id.to_owned(), CallPhase::Executing);
        call.tool_call_id = Some(start.tool_call_id.clone());
        call.parent_key = parent_key;
        call.input = CallInput {
            announced: Value::Null,
            started: start.args,
            prepared: Value::Null,
        };
        call.start_sequence = Some(step.sequence);
        call.started_at_ms = Some(started_at);
        call.resolved_name = Some(start.tool_name);
        let created = self.create(step, start.key, call)?;
        if existing {
            self.table
                .repoint(turn_id, &start.tool_call_id, &created.runtime_item_key);
        }
        Ok(Some(created))
    }

    pub fn execution_update(
        &mut self,
        step: &CallStep,
        tool_call_id: &str,
        args: &Value,
        partial_result: &Value,
    ) -> Result<Option<OutstandingCall>, CompactError> {
        let Some(call) = self.resolve(step, tool_call_id, executing).cloned() else {
            return Ok(self.ignore_unknown());
        };
        if call.phase != CallPhase::Executing {
            return Ok(self.ignore_late());
        }
        let mut progressed =
            self.table
                .replace_slot(&call, CallSlotField::Progress, partial_result)?;
        self.table.counters.progress_replacements += 1;
        if call.input.prepared.is_null() {
            progressed.input.prepared = args.clone();
            progressed = self.update(step, progressed)?;
        }
        self.sequence = step.sequence;
        Ok(Some(progressed))
    }

    pub fn hook(
        &mut self,
        step: &CallStep,
        tool_call_id: &str,
        record: &Value,
    ) -> Result<Option<OutstandingCall>, CompactError> {
        let Some(call) = self.resolve(step, tool_call_id, still_open).cloned() else {
            return Ok(self.ignore_unknown());
        };
        if terminal(&call) {
            return Ok(self.ignore_late());
        }
        let hooked = self
            .table
            .replace_slot(&call, CallSlotField::Hook, record)?;
        self.sequence = step.sequence;
        Ok(Some(hooked))
    }

    pub fn execution_end(
        &mut self,
        step: &CallStep,
        tool_call_id: &str,
        result: &Value,
        is_error: bool,
    ) -> Result<Option<OutstandingCall>, CompactError> {
        let Some(call) = self.resolve(step, tool_call_id, still_open).cloned() else {
            return Ok(self.ignore_unknown());
        };
        if terminal(&call) && !matches!(call.outcome, Some(CallOutcome::Abandoned)) {
            return Ok(self.ignore_late());
        }
        let mut ended = self
            .table
            .replace_slot(&call, CallSlotField::Result, result)?;
        ended.phase = TERMINAL_PHASE;
        ended.outcome = Some(CallOutcome::Result {
            is_error,
            source: CallOutcomeSource::ExecutionEnd,
        });
        ended.join = if orphaned(&self.table, &call) || call.join == CallJoin::Carrierless {
            CallJoin::Carrierless
        } else {
            CallJoin::Pending
        };
        self.update(step, ended).map(Some)
    }

    pub fn settle(
        &mut self,
        step: &CallStep,
        tool_call_id: &str,
        is_error: bool,
    ) -> Result<Option<CallProjection>, CompactError> {
        let Some(call) = self.resolve(step, tool_call_id, awaiting_carrier).cloned() else {
            return Ok(None);
        };
        let projection = settle_call(&mut self.table, &call, is_error)?;
        self.sequence = step.sequence;
        Ok(Some(projection))
    }

    pub fn turn_end(&mut self, step: &CallStep) -> Result<(), CompactError> {
        let Some(turn_id) = step.scope.turn_id.as_deref() else {
            return Ok(());
        };
        for mut call in self.table.of_turn(turn_id) {
            if call.start_sequence.is_none() && !terminal(&call) {
                not_executed_patch().apply(&mut call);
                self.update(step, call)?;
            }
        }
        Ok(())
    }

    pub fn agent_settled(&mut self, step: &CallStep) -> Result<(), CompactError> {
        for mut call in self.table.all() {
            if let Some(patch) = settled_patch(&call) {
                patch.apply(&mut call);
                self.update(step, call)?;
            }
        }
        Ok(())
    }

    pub fn freeze(&self) -> Result<FrozenCalls, CompactError> {
        Ok(FrozenCalls {
            sequence: self.sequence,
            reference: self.table.freeze()?,
            counters: self.table.snapshot(),
        })
    }

    pub fn release(&self, frozen: &FrozenCalls) -> Result<(), CompactError> {
        self.table.release(frozen.reference.as_ref())
    }

    pub fn entries<'a>(
        &'a self,
        reference: Option<&'a ValueRef>,
        valid: &'a (dyn Fn() -> Result<(), CompactError> + Sync),
    ) -> BoxFuture<'a, Result<Vec<(String, OutstandingCall)>, CompactError>> {
        self.table.entries_of(reference, valid)
    }

    fn resolve(
        &self,
        step: &CallStep,
        tool_call_id: &str,
        open: impl Fn(&OutstandingCall) -> bool,
    ) -> Option<&OutstandingCall> {
        let turn_id = step.scope.turn_id.as_deref();
        self.table
            .oldest_by_id(tool_call_id, turn_id, open)
            .or_else(|| self.lookup(turn_id, tool_call_id))
    }

    fn lookup(&self, turn_id: Option<&str>, tool_call_id: &str) -> Option<&OutstandingCall> {
        self.table
            .by_turn_id(turn_id, Some(tool_call_id))
            .or_else(|| self.table.newest_by_id(tool_call_id))
    }

    fn ignore_unknown(&mut self) -> Option<OutstandingCall> {
        self.table.counters.unknown += 1;
        None
    }

    fn ignore_late(&mut self) -> Option<OutstandingCall> {
        self.table.counters.late += 1;
        None
    }

    fn create(
        &mut self,
        step: &CallStep,
        key: Option<String>,
        mut call: OutstandingCall,
    ) -> Result<OutstandingCall, CompactError> {
        if self.minted_at != Some(step.sequence) {
            self.minted_at = Some(step.sequence);
            self.minted_k = 0;
        } else {
            self.minted_k += 1;
        }
        call.runtime_item_key =
            key.unwrap_or_else(|| runtime_item_key(&step.scope, step.sequence, self.minted_k));
        self.table.put(call.clone(), step.sequence, self.minted_k)?;
        self.sequence = step.sequence;
        Ok(call)
    }

    fn update(
        &mut self,
        step: &CallStep,
        call: OutstandingCall,
    ) -> Result<OutstandingCall, CompactError> {
        self.table.reput(call.clone())?;
        self.sequence = step.sequence;
        Ok(call)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact_content_store::CompactContentStoreOptions;
    use serde_json::json;

    #[tokio::test]
    async fn reduces_bridge_start_announce_execute_and_settle_fifo() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = CompactContentStore::new(CompactContentStoreOptions {
            dir: dir.path().join("epoch").display().to_string(),
            ..Default::default()
        });
        let mut calls = CompactSessionCalls::new(store.clone());
        let scope = CallScope {
            session_epoch: "E".into(),
            turn_id: Some("T1".into()),
        };

        // Bridge-started call arrives before block announcement
        let started = calls
            .execution_start(
                &CallStep {
                    sequence: 1,
                    scope: scope.clone(),
                },
                ExecutionStart {
                    tool_call_id: "tc1".into(),
                    tool_name: "bash".into(),
                    args: json!({"cmd": "ls"}),
                    parent_tool_call_id: None,
                    key: Some("exe:1".into()),
                },
            )
            .expect("exec start")
            .expect("call");
        assert_eq!(started.runtime_item_key, "exe:1");

        // Block joins bridge-started call
        calls
            .tool_start(
                &CallStep {
                    sequence: 2,
                    scope: scope.clone(),
                },
                AnnouncedBlock {
                    message_id: "m1".into(),
                    content_index: 0,
                    id: "tc1".into(),
                    name: "bash".into(),
                    arguments: json!({}),
                    resolved_name: None,
                    correlation: None,
                },
            )
            .expect("tool_start");
        let cur = calls.get("exe:1").expect("still exe:1");
        assert_eq!(cur.owning_message_id.as_deref(), Some("m1"));
        assert_eq!(cur.phase, CallPhase::Executing);

        calls
            .execution_end(
                &CallStep {
                    sequence: 3,
                    scope: scope.clone(),
                },
                "tc1",
                &json!({"ok": true}),
                false,
            )
            .expect("exec end");
        let proj = calls
            .settle(&CallStep { sequence: 4, scope }, "tc1", false)
            .expect("settle")
            .expect("projection");
        assert_eq!(proj.runtime_item_key, "exe:1");
        assert_eq!(calls.table.snapshot().live, 0);
        store.retire().await.expect("retire");
    }
}
