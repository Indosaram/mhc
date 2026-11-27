//! Port of senpi `packages/coding-agent/src/modes/rpc/loop-hook-call.ts`.
//!
//! The loop facts a hand's runner sees during one loop-hook call (S33): the loop session's id,
//! read-only (no transcript path: the transcript lives where the loop runs), the session port, the
//! call's own abort signal, and idle. Outside the call no loop-side member is bound. After the
//! call ended, its work keeps what `LoopHookCallEndedError` lists; the session's identity is
//! bound to the call only.

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use maho_ai::utils::abort::{AbortController, AbortSignal};
use maho_core::execution::hand_wake_route::LoopWakeRoute;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SentLoopMessage {
    pub message: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppendedLoopEntry {
    pub custom_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SentLoopUserMessage {
    pub content: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoopNotice {
    pub message: String,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

/// The session port's read half (r3 N2): the loop facts a call carries beyond the session's id.
/// The loop owns the session; it sends its current leaf and the entries THIS hand authored earlier
/// in the session, after the last compaction only. A hand never sees entries the loop authored.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CarriedSession {
    pub leaf_id: Option<String>,
    pub hand_entries: Vec<Value>,
}

/// What a loop-hook call's handlers did beyond their result, which the loop does in turn.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LoopHookEffects {
    pub sent: Vec<SentLoopMessage>,
    pub notices: Vec<LoopNotice>,
    pub entries: Vec<AppendedLoopEntry>,
    pub user_messages: Vec<SentLoopUserMessage>,
}

/// What a loop-hook call binds its handlers' facts to: the loop session's state the call carries.
#[derive(Debug, Clone)]
pub struct LoopHookCallBinding {
    pub session_id: String,
    pub cwd: String,
    pub idle: bool,
    pub carried: Option<CarriedSession>,
    pub ends_turn: bool,
}

/// The deliberate refusal after a loop-hook call ended (option A).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("this loop-hook call has ended; {member} is bound to that call only")]
pub struct LoopHookCallEndedError {
    pub member: String,
}

impl LoopHookCallEndedError {
    pub fn new(member: impl Into<String>) -> Self {
        Self {
            member: member.into(),
        }
    }
}

pub type LateLoopRoute = Arc<dyn Fn(&str) -> LoopWakeRoute + Send + Sync>;

/// Read-only view of the loop's carried session during and (partially) after a call.
#[derive(Clone)]
pub struct LoopReadOnlySession {
    session_id: String,
    cwd: String,
    carried: Option<CarriedSession>,
    live: Arc<AtomicBool>,
    token: u64,
}

impl LoopReadOnlySession {
    fn ensure_live(&self, member: &str) -> Result<(), LoopHookCallEndedError> {
        if self.live.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(LoopHookCallEndedError::new(format!(
                "sessionManager.{member}"
            )))
        }
    }

    fn ensure_own_work(&self, member: &str) -> Result<(), LoopHookCallEndedError> {
        let same_call = CURRENT_CALL
            .try_with(|ctx| ctx.facts.token == self.token)
            .unwrap_or(false);
        if self.live.load(Ordering::SeqCst) || same_call {
            Ok(())
        } else {
            Err(LoopHookCallEndedError::new(format!(
                "sessionManager.{member}"
            )))
        }
    }

    pub fn get_session_id(&self) -> Result<&str, LoopHookCallEndedError> {
        self.ensure_live("getSessionId")?;
        Ok(&self.session_id)
    }

    pub fn get_session_file(&self) -> Result<Option<&str>, LoopHookCallEndedError> {
        self.ensure_live("getSessionFile")?;
        Ok(None)
    }

    pub fn get_cwd(&self) -> Result<&str, LoopHookCallEndedError> {
        self.ensure_live("getCwd")?;
        Ok(&self.cwd)
    }

    pub fn get_leaf_id(&self) -> Result<Option<&str>, String> {
        let Some(ref carried) = self.carried else {
            return Err("sessionManager.getLeafId is loop-side: a loop-hook call on a hand carries only the session's id".to_owned());
        };
        self.ensure_own_work("getLeafId")
            .map_err(|e| e.to_string())?;
        Ok(carried.leaf_id.as_deref())
    }

    pub fn reduce_history<S>(
        &self,
        initial: impl FnOnce() -> S,
        add: impl FnMut(&mut S, &Value),
    ) -> Result<S, String> {
        let Some(ref carried) = self.carried else {
            return Err("sessionManager.reduceHistory is loop-side: a loop-hook call on a hand carries only the session's id".to_owned());
        };
        self.ensure_own_work("reduceHistory")
            .map_err(|e| e.to_string())?;
        let mut state = initial();
        let mut add = add;
        for entry in &carried.hand_entries {
            add(&mut state, entry);
        }
        Ok(state)
    }
}

/// Facts visible to extension handlers during a loop-hook call.
#[derive(Clone)]
pub struct LoopHookCallFacts {
    pub token: u64,
    pub session_id: String,
    pub session_manager: LoopReadOnlySession,
    pub signal: AbortSignal,
    idle: bool,
    ends_turn: bool,
    live: Arc<AtomicBool>,
    latest: Arc<Mutex<HashMap<String, u64>>>,
    effects: Arc<Mutex<LoopHookEffects>>,
    user_messages_sent: Arc<AtomicUsize>,
    late: LateLoopRoute,
}

impl LoopHookCallFacts {
    pub fn is_idle(&self) -> bool {
        if self.live.load(Ordering::SeqCst) || !self.ends_turn {
            self.idle
        } else {
            self.latest
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(&self.session_id)
                .copied()
                == Some(self.token)
        }
    }

    pub fn send_message(&self, message: Value, options: Option<Value>) {
        if self.live.load(Ordering::SeqCst) {
            self.effects
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .sent
                .push(SentLoopMessage { message, options });
        } else {
            let route = (self.late)(&self.session_id);
            (route.send_message)(message, options);
        }
    }

    pub fn notify(&self, message: &str, kind: Option<&str>) {
        if self.live.load(Ordering::SeqCst) {
            self.effects
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .notices
                .push(LoopNotice {
                    message: message.to_owned(),
                    kind: kind.map(str::to_owned),
                });
        } else {
            let route = (self.late)(&self.session_id);
            (route.notify)(message, kind);
        }
    }

    pub fn append_entry(&self, custom_type: &str, data: Option<Value>) {
        if self.live.load(Ordering::SeqCst) {
            self.effects
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entries
                .push(AppendedLoopEntry {
                    custom_type: custom_type.to_owned(),
                    data,
                });
        } else {
            let route = (self.late)(&self.session_id);
            (route.append_entry)(custom_type, data);
        }
    }

    pub fn send_user_message(&self, content: Value, options: Option<Value>) {
        self.user_messages_sent.fetch_add(1, Ordering::SeqCst);
        if self.live.load(Ordering::SeqCst) {
            self.effects
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .user_messages
                .push(SentLoopUserMessage { content, options });
        } else {
            let route = (self.late)(&self.session_id);
            (route.send_user_message)(content, options);
        }
    }

    pub fn has_pending_messages(&self) -> bool {
        self.user_messages_sent.load(Ordering::SeqCst) > 0
    }
}

#[derive(Clone)]
struct CallScope {
    facts: Arc<LoopHookCallFacts>,
}

tokio::task_local! {
    static CURRENT_CALL: CallScope;
}

pub struct LoopHookCall {
    late: LateLoopRoute,
    latest: Arc<Mutex<HashMap<String, u64>>>,
    next_token: AtomicU64,
}

impl LoopHookCall {
    pub fn new(late: LateLoopRoute) -> Self {
        Self {
            late,
            latest: Arc::new(Mutex::new(HashMap::new())),
            next_token: AtomicU64::new(1),
        }
    }

    pub fn current(&self) -> Option<Arc<LoopHookCallFacts>> {
        CURRENT_CALL
            .try_with(|scope| {
                scope
                    .facts
                    .live
                    .load(Ordering::SeqCst)
                    .then(|| Arc::clone(&scope.facts))
            })
            .ok()
            .flatten()
    }

    /// The call's facts whose work runs in this async context, also after that call ended.
    pub fn active_or_late(&self) -> Option<Arc<LoopHookCallFacts>> {
        CURRENT_CALL.try_with(|scope| Arc::clone(&scope.facts)).ok()
    }

    pub async fn run<T, F, Fut>(&self, binding: LoopHookCallBinding, dispatch: F) -> T
    where
        F: FnOnce(Arc<Mutex<LoopHookEffects>>) -> Fut,
        Fut: Future<Output = T>,
    {
        let token = self.next_token.fetch_add(1, Ordering::SeqCst);
        self.latest
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(binding.session_id.clone(), token);
        let live = Arc::new(AtomicBool::new(true));
        let controller = AbortController::new();
        let effects = Arc::new(Mutex::new(LoopHookEffects::default()));
        let facts = Arc::new(LoopHookCallFacts {
            token,
            session_id: binding.session_id.clone(),
            session_manager: LoopReadOnlySession {
                session_id: binding.session_id,
                cwd: binding.cwd,
                carried: binding.carried,
                live: Arc::clone(&live),
                token,
            },
            signal: controller.signal(),
            idle: binding.idle,
            ends_turn: binding.ends_turn,
            live: Arc::clone(&live),
            latest: Arc::clone(&self.latest),
            effects: Arc::clone(&effects),
            user_messages_sent: Arc::new(AtomicUsize::new(0)),
            late: Arc::clone(&self.late),
        });
        struct FinishGuard {
            live: Arc<AtomicBool>,
            controller: AbortController,
        }
        impl Drop for FinishGuard {
            fn drop(&mut self) {
                self.live.store(false, Ordering::SeqCst);
                self.controller.abort(None);
            }
        }
        let _finish = FinishGuard { live, controller };
        CURRENT_CALL
            .scope(CallScope { facts }, dispatch(effects))
            .await
    }
}

pub fn create_loop_hook_call(late: LateLoopRoute) -> LoopHookCall {
    LoopHookCall::new(late)
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_core::execution::hand_wake_route::{HandWake, HandWakeSource, late_loop_route};
    use serde_json::json;

    #[tokio::test]
    async fn collects_live_effects_and_routes_late_work_after_call_ends() {
        let late_wakes = Arc::new(Mutex::new(Vec::<HandWake>::new()));
        let wakes_sink = Arc::clone(&late_wakes);
        let route = late_loop_route(
            Some(Arc::new(move |wake| {
                wakes_sink
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(wake);
            })),
            Arc::new(|_| {}),
        );
        let call = create_loop_hook_call(route);
        assert!(call.current().is_none());

        let call_ref = &call;
        let (kept_facts, effects) = call
            .run(
                LoopHookCallBinding {
                    session_id: "s-1".into(),
                    cwd: "/repo".into(),
                    idle: false,
                    carried: Some(CarriedSession {
                        leaf_id: Some("leaf-1".into()),
                        hand_entries: vec![json!({"id": "e1", "n": 2})],
                    }),
                    ends_turn: true,
                },
                |effects| async move {
                    let facts = call_ref.current().expect("current");
                    assert_eq!(facts.session_manager.get_session_id().expect("id"), "s-1");
                    assert_eq!(facts.session_manager.get_cwd().expect("cwd"), "/repo");
                    facts.send_message(json!({"text": "live"}), None);
                    facts.notify("hi", Some("info"));
                    facts.append_entry("custom", Some(json!(1)));
                    (facts, effects)
                },
            )
            .await;

        assert!(call.current().is_none());
        assert!(kept_facts.signal.aborted());
        // ends_turn + no newer call => is_idle() is true now.
        assert!(kept_facts.is_idle());
        // Identity is refused once ended.
        assert!(matches!(
            kept_facts.session_manager.get_session_id(),
            Err(LoopHookCallEndedError { .. })
        ));
        // Live effects were captured.
        let collected = effects
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        assert_eq!(collected.sent.len(), 1);
        assert_eq!(collected.notices.len(), 1);
        assert_eq!(collected.entries.len(), 1);

        // Late writes go to the wake route.
        kept_facts.send_message(json!({"text": "late"}), None);
        let wakes = late_wakes.lock().unwrap_or_else(PoisonError::into_inner);
        assert_eq!(wakes.len(), 1);
        assert_eq!(wakes[0].source, HandWakeSource::Message);
        assert_eq!(wakes[0].payload["sessionId"], "s-1");
    }
}
