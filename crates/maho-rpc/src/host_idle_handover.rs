//! The conditional idle handover, as the RUNNING host performs it (senpi `host-idle-handover.ts`).
//!
//! `host handoff --when idle` names the generation it means (`if_instance_id`, `if_generation`), the runtime it
//! wants serving (`target_runtime_build_id`) and one `operation_id`. The host owns the operation from then on:
//! it checks it IS the named generation (`stale_generation` otherwise), stops admitting new input (`admits`),
//! waits for its next safe idle point (no deadline: a long turn delays the handover, it is never aborted for it),
//! then starts the successor through the ordinary generation handoff. A successor that does not come up leaves
//! this generation serving and admits input again (`handover_blocked`).
//!
//! A repeated `operation_id` with the same terms answers with the operation that already exists; the same id
//! with other terms is refused (`operation_conflict`). Another id for the same target joins the pending
//! operation; another target is refused (`handover_in_progress`) until the pending operation ends.
use crate::host_launch_spec::HostLifecyclePolicyInput;
use futures::future::BoxFuture;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

/// Set on a successor launched by an idle handover: the runtime it must report. A successor that computes
/// another id exits before it listens, so the socket never moves to the wrong runtime.
pub const EXPECTED_RUNTIME_BUILD_ID_ENV: &str = "SENPI_RPC_HOST_EXPECTED_RUNTIME_BUILD_ID";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandoverState {
    Pending,
    Switching,
    Completed,
    Blocked,
}
impl HandoverState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "handover_pending",
            Self::Switching => "handover_switching",
            Self::Completed => "handover_completed",
            Self::Blocked => "handover_blocked",
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandoverLaunch {
    pub command: String,
    pub args: Vec<String>,
}
#[derive(Debug, Clone, PartialEq)]
pub struct IdleHandoverRequest {
    pub operation_id: String,
    pub if_instance_id: String,
    pub if_generation: f64,
    pub target_runtime_build_id: String,
    /// How the CLI that asked re-enters its own runtime as a supervisor: the successor runs THAT runtime.
    pub launch: HandoverLaunch,
    pub host_args: Vec<String>,
    /// The complete, already allowlisted daemon environment of the CLI that asked.
    pub env: BTreeMap<String, String>,
    pub policy: Option<HostLifecyclePolicyInput>,
}
#[derive(Debug, Clone, PartialEq)]
pub struct HandoverSuccessor {
    pub pid: u32,
    pub instance_id: String,
    pub generation: f64,
    pub runtime_build_id: Option<String>,
}
#[derive(Debug, Clone, PartialEq)]
pub struct HandoverView {
    pub operation_id: String,
    pub state: HandoverState,
    pub target_runtime_build_id: String,
    pub reason: Option<String>,
    pub successor: Option<HandoverSuccessor>,
}
#[derive(Debug, Clone, PartialEq)]
pub enum HandoverAnswer {
    Operation(HandoverView),
    Refused {
        reason: &'static str,
        detail: Option<String>,
    },
}
#[derive(Debug, Clone, PartialEq)]
pub struct HostIdentityView {
    pub instance_id: String,
    pub generation: f64,
    pub runtime_build_id: Option<String>,
}
/// A successor that came up, or the reason it did not.
pub type HandoverOutcome = Result<HandoverSuccessor, String>;

pub type PerformIdleHandover =
    Box<dyn Fn(IdleHandoverRequest) -> BoxFuture<'static, HandoverOutcome> + Send + Sync>;

pub struct HostIdleHandoverOptions {
    pub identity: Box<dyn Fn() -> HostIdentityView + Send + Sync>,
    pub is_idle: Box<dyn Fn() -> bool + Send + Sync>,
    pub perform: PerformIdleHandover,
    /// How often a pending operation re-checks for idle between request settlements.
    pub poll_ms: Option<u64>,
    pub log: Option<HandoverLog>,
}

pub type HandoverLog = Box<dyn Fn(&str) + Send + Sync>;

/// Commands that would start, extend or queue model work. Everything else stays admitted.
pub const NEW_WORK_COMMANDS: [&str; 12] = [
    "prompt",
    "steer",
    "follow_up",
    "send_custom_message",
    "continue_from_leaf",
    "resume_session",
    "append_user_message",
    "bash",
    "compact",
    "wake",
    "edit_assistant_message",
    "edit_user_message",
];
const DEFAULT_POLL_MS: u64 = 200;

struct Operation {
    request: IdleHandoverRequest,
    view: HandoverView,
}
struct Inner {
    options: HostIdleHandoverOptions,
    operation: Mutex<Option<Operation>>,
    timer: Mutex<Option<tokio::task::JoinHandle<()>>>,
}
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Clone)]
pub struct HostIdleHandover {
    inner: Arc<Inner>,
}
impl HostIdleHandover {
    pub fn new(options: HostIdleHandoverOptions) -> Self {
        Self {
            inner: Arc::new(Inner {
                options,
                operation: Mutex::new(None),
                timer: Mutex::new(None),
            }),
        }
    }
    /// The operation this host holds, for `get_protocol_info` and therefore `host status`.
    pub fn view(&self) -> Option<HandoverView> {
        lock(&self.inner.operation)
            .as_ref()
            .map(|operation| operation.view.clone())
    }
    /// False while a handover holds new work back; a blocked one admits again.
    pub fn admits(&self, command_type: &str) -> bool {
        let state = lock(&self.inner.operation)
            .as_ref()
            .map(|operation| operation.view.state);
        state.is_none_or(|state| {
            state == HandoverState::Blocked || !NEW_WORK_COMMANDS.contains(&command_type)
        })
    }
    pub async fn begin(&self, request: IdleHandoverRequest) -> HandoverAnswer {
        {
            let mut guard = lock(&self.inner.operation);
            if let Some(current) = guard.as_ref() {
                if current.view.operation_id == request.operation_id {
                    return if same_terms(&current.request, &request) {
                        HandoverAnswer::Operation(current.view.clone())
                    } else {
                        HandoverAnswer::Refused {
                            reason: "operation_conflict",
                            detail: Some("this operation id names other terms".into()),
                        }
                    };
                }
                if current.view.state != HandoverState::Blocked {
                    return if current.request.target_runtime_build_id
                        == request.target_runtime_build_id
                    {
                        HandoverAnswer::Operation(current.view.clone())
                    } else {
                        HandoverAnswer::Refused {
                            reason: "handover_in_progress",
                            detail: Some(current.view.operation_id.clone()),
                        }
                    };
                }
            }
            let this = (self.inner.options.identity)();
            if this.instance_id != request.if_instance_id
                || this.generation != request.if_generation
            {
                return HandoverAnswer::Refused {
                    reason: "stale_generation",
                    detail: Some(format!(
                        "this host is {} generation {}",
                        this.instance_id, this.generation
                    )),
                };
            }
            if this.runtime_build_id.as_deref() == Some(request.target_runtime_build_id.as_str()) {
                return HandoverAnswer::Refused {
                    reason: "already_current",
                    detail: this.runtime_build_id,
                };
            }
            let view = HandoverView {
                operation_id: request.operation_id.clone(),
                state: HandoverState::Pending,
                target_runtime_build_id: request.target_runtime_build_id.clone(),
                reason: None,
                successor: None,
            };
            *guard = Some(Operation {
                request: request.clone(),
                view,
            });
        }
        self.log(&format!(
            "handover {} pending: no new work is admitted",
            request.operation_id
        ));
        if self.idle_now() {
            self.switch_over().await;
        } else {
            let this = self.clone();
            let period =
                Duration::from_millis(self.inner.options.poll_ms.unwrap_or(DEFAULT_POLL_MS).max(1));
            let task = tokio::spawn(async move {
                let mut ticker = tokio::time::interval(period);
                loop {
                    ticker.tick().await;
                    if this.is_pending() && this.idle_now() {
                        this.switch_over().await;
                        return;
                    }
                    if !this.is_pending() {
                        return;
                    }
                }
            });
            if let Some(previous) = lock(&self.inner.timer).replace(task) {
                previous.abort();
            }
        }
        match self.view() {
            Some(view) => HandoverAnswer::Operation(view),
            None => HandoverAnswer::Refused {
                reason: "handover_in_progress",
                detail: None,
            },
        }
    }
    /// Re-checks a pending operation; the host calls it whenever a request settles.
    pub fn observe(&self) {
        if self.is_pending() && self.idle_now() {
            let this = self.clone();
            tokio::spawn(async move {
                this.switch_over().await;
            });
        }
    }
    pub fn dispose(&self) {
        self.stop_timer();
    }
    fn is_pending(&self) -> bool {
        lock(&self.inner.operation)
            .as_ref()
            .is_some_and(|operation| operation.view.state == HandoverState::Pending)
    }
    fn idle_now(&self) -> bool {
        (self.inner.options.is_idle)()
    }
    fn log(&self, line: &str) {
        if let Some(log) = &self.inner.options.log {
            log(line);
        }
    }
    async fn switch_over(&self) {
        let request = {
            let mut guard = lock(&self.inner.operation);
            let Some(operation) = guard
                .as_mut()
                .filter(|operation| operation.view.state == HandoverState::Pending)
            else {
                return;
            };
            operation.view.state = HandoverState::Switching;
            operation.request.clone()
        };
        self.stop_timer();
        let outcome = (self.inner.options.perform)(request).await;
        let line = {
            let mut guard = lock(&self.inner.operation);
            let Some(operation) = guard.as_mut() else {
                return;
            };
            match outcome {
                Ok(successor) => {
                    operation.view.state = HandoverState::Completed;
                    operation.view.successor = Some(successor);
                }
                Err(reason) => {
                    operation.view.state = HandoverState::Blocked;
                    operation.view.reason = Some(reason);
                }
            }
            format!(
                "handover {} {}",
                operation.view.operation_id,
                operation.view.state.as_str()
            )
        };
        self.log(&line);
    }
    fn stop_timer(&self) {
        if let Some(task) = lock(&self.inner.timer).take() {
            task.abort();
        }
    }
}
fn same_terms(a: &IdleHandoverRequest, b: &IdleHandoverRequest) -> bool {
    a.target_runtime_build_id == b.target_runtime_build_id
        && a.if_instance_id == b.if_instance_id
        && a.if_generation == b.if_generation
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    fn request(operation: &str, target: &str) -> IdleHandoverRequest {
        IdleHandoverRequest {
            operation_id: operation.into(),
            if_instance_id: "i".into(),
            if_generation: 1.,
            target_runtime_build_id: target.into(),
            launch: HandoverLaunch {
                command: "senpi".into(),
                args: vec![],
            },
            host_args: vec![],
            env: BTreeMap::new(),
            policy: None,
        }
    }
    fn successor() -> HandoverSuccessor {
        HandoverSuccessor {
            pid: 9,
            instance_id: "next".into(),
            generation: 2.,
            runtime_build_id: Some("new".into()),
        }
    }
    fn handover(idle: Arc<AtomicBool>, outcome: HandoverOutcome) -> HostIdleHandover {
        HostIdleHandover::new(HostIdleHandoverOptions {
            identity: Box::new(|| HostIdentityView {
                instance_id: "i".into(),
                generation: 1.,
                runtime_build_id: Some("old".into()),
            }),
            is_idle: Box::new(move || idle.load(Ordering::SeqCst)),
            perform: Box::new(move |_| {
                let outcome = outcome.clone();
                Box::pin(async move { outcome })
            }),
            poll_ms: Some(3_600_000),
            log: None,
        })
    }
    #[test]
    fn resume_session_is_new_work_and_everything_else_stays_admitted() {
        assert!(NEW_WORK_COMMANDS.contains(&"resume_session"));
        let held = handover(Arc::new(AtomicBool::new(false)), Ok(successor()));
        assert!(held.admits("resume_session"));
    }
    #[tokio::test]
    async fn an_idle_host_switches_at_once_and_a_repeated_id_answers_the_same_operation() {
        let host = handover(Arc::new(AtomicBool::new(true)), Ok(successor()));
        let HandoverAnswer::Operation(view) = host.begin(request("op", "new")).await else {
            panic!("refused")
        };
        assert_eq!(view.state, HandoverState::Completed);
        assert_eq!(view.successor, Some(successor()));
        assert_eq!(
            host.begin(request("op", "new")).await,
            HandoverAnswer::Operation(view)
        );
        assert!(matches!(
            host.begin(request("op", "other")).await,
            HandoverAnswer::Refused {
                reason: "operation_conflict",
                ..
            }
        ));
    }
    #[tokio::test]
    async fn a_busy_host_holds_new_work_back_until_observe_finds_it_idle() {
        let idle = Arc::new(AtomicBool::new(false));
        let host = handover(idle.clone(), Ok(successor()));
        let HandoverAnswer::Operation(view) = host.begin(request("op", "new")).await else {
            panic!("refused")
        };
        assert_eq!(view.state, HandoverState::Pending);
        assert!(
            !host.admits("prompt") && !host.admits("resume_session") && host.admits("get_state")
        );
        assert!(matches!(
            host.begin(request("other", "elsewhere")).await,
            HandoverAnswer::Refused {
                reason: "handover_in_progress",
                ..
            }
        ));
        idle.store(true, Ordering::SeqCst);
        host.observe();
        let wait = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if host
                    .view()
                    .is_some_and(|view| view.state == HandoverState::Completed)
                {
                    return;
                }
                tokio::task::yield_now().await;
            }
        });
        wait.await
            .unwrap_or_else(|_| panic!("handover never completed"));
        host.dispose();
    }
    #[tokio::test]
    async fn a_failed_successor_blocks_and_admits_input_again_and_stale_or_current_is_refused() {
        let host = handover(
            Arc::new(AtomicBool::new(true)),
            Err("successor_unavailable: x".into()),
        );
        let HandoverAnswer::Operation(view) = host.begin(request("op", "new")).await else {
            panic!("refused")
        };
        assert_eq!(view.state, HandoverState::Blocked);
        assert_eq!(view.reason.as_deref(), Some("successor_unavailable: x"));
        assert!(host.admits("prompt"));
        let fresh = handover(Arc::new(AtomicBool::new(true)), Ok(successor()));
        let mut stale = request("op2", "new");
        stale.if_generation = 7.;
        assert!(matches!(
            fresh.begin(stale).await,
            HandoverAnswer::Refused {
                reason: "stale_generation",
                ..
            }
        ));
        assert!(matches!(
            fresh.begin(request("op3", "old")).await,
            HandoverAnswer::Refused {
                reason: "already_current",
                ..
            }
        ));
    }
}
