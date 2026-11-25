//! Event-driven bounded turn settle wait (`thread/control/wait-settled.ts`).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::oneshot;

use super::super::contracts::results::ThreadWaitOutcome;
use super::super::tools::ports::{
    BoxFuture, SessionWatchHandlers, SessionWatchOutcome, SessionWatchResync,
    SessionWatchStatusChange,
};
use super::port::ControlRefusal;

const QUIET_MS: u64 = 3_000;

fn in_progress(status: &str) -> bool {
    matches!(status, "running" | "compacting" | "opening")
}

fn is_run_record(kind: &str) -> bool {
    matches!(
        kind,
        "agent_start"
            | "turn_start"
            | "message_start"
            | "message_update"
            | "message_end"
            | "tool_execution_start"
            | "tool_execution_update"
            | "tool_execution_end"
            | "turn_end"
            | "agent_end"
            | "auto_retry_start"
            | "auto_retry_end"
    )
}

fn turn_of(record: &Value) -> Option<String> {
    record
        .get("turnId")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

struct WaitState {
    started: Instant,
    status: String,
    running: bool,
    live: bool,
    turn: Option<String>,
    settled: bool,
    tx: Option<oneshot::Sender<ThreadWaitOutcome>>,
    quiet_notify: Arc<tokio::sync::Notify>,
}

impl WaitState {
    fn finish(&mut self, outcome: &str, at: &str, turn_id: Option<String>) {
        if self.settled {
            return;
        }
        self.settled = true;
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(ThreadWaitOutcome {
                outcome: outcome.to_owned(),
                status: at.to_owned(),
                waited_ms: self.started.elapsed().as_millis() as u64,
                turn_id,
            });
        }
    }

    fn run_seen(&mut self, turn_id: Option<String>) {
        self.running = true;
        if let Some(t) = turn_id {
            self.turn = Some(t);
        }
        self.quiet_notify.notify_waiters();
    }
}

struct WatchSink {
    state: Arc<Mutex<WaitState>>,
}

impl SessionWatchHandlers for WatchSink {
    fn record(&self, record: &Value) {
        let Some(kind) = record.get("type").and_then(Value::as_str) else {
            return;
        };
        let mut st = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if kind == "agent_settled" {
            let at = if record.get("reason").and_then(Value::as_str) == Some("session_closed") {
                "closed"
            } else {
                "idle"
            };
            st.status = at.to_owned();
            let turn = turn_of(record).or_else(|| st.turn.clone());
            st.finish("settled", at, turn);
        } else if is_run_record(kind) {
            st.run_seen(turn_of(record));
        }
    }

    fn status(&self, change: SessionWatchStatusChange) {
        let mut st = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        st.live = true;
        st.status = change.to.clone();
        if in_progress(&change.to) {
            st.run_seen(None);
        } else if st.running || change.from.as_deref().is_some_and(in_progress) {
            let turn = st.turn.clone();
            st.finish("settled", &change.to, turn);
        }
    }

    fn resynced(&self, resync: SessionWatchResync) {
        let mut st = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        st.live = true;
        st.status = resync.state.status.clone();
        if let Some(settled_turn) = resync.settled_turns.last() {
            let status = st.status.clone();
            st.finish("settled", &status, Some(settled_turn.clone()));
        } else if in_progress(&resync.state.status) {
            st.run_seen(resync.state.current_turn_id);
        } else if st.running {
            let status = st.status.clone();
            let turn = st.turn.clone();
            st.finish("settled", &status, turn);
        }
    }
}

pub async fn wait_for_turn_end<F, R>(
    open: F,
    refused: R,
    timeout_ms: u64,
) -> Result<ThreadWaitOutcome, super::port::ControlError>
where
    F: FnOnce(Arc<dyn SessionWatchHandlers>) -> BoxFuture<Result<SessionWatchOutcome, String>>,
    R: FnOnce(&str) -> ControlRefusal,
{
    let (tx, mut rx) = oneshot::channel::<ThreadWaitOutcome>();
    let quiet_notify = Arc::new(tokio::sync::Notify::new());
    let state = Arc::new(Mutex::new(WaitState {
        started: Instant::now(),
        status: "unknown".to_owned(),
        running: false,
        live: false,
        turn: None,
        settled: false,
        tx: Some(tx),
        quiet_notify: quiet_notify.clone(),
    }));
    let sink: Arc<dyn SessionWatchHandlers> = Arc::new(WatchSink {
        state: state.clone(),
    });

    let open_fut = open(sink);
    let timeout_sleep = tokio::time::sleep(Duration::from_millis(timeout_ms));
    tokio::pin!(open_fut);
    tokio::pin!(timeout_sleep);

    let watch = tokio::select! {
        res = &mut open_fut => {
            match res {
                Ok(SessionWatchOutcome::Ok(w)) => w,
                Ok(SessionWatchOutcome::Refused { reason }) => {
                    return Err(refused(&reason).into());
                }
                Err(err) => return Err(super::port::ControlError::Transport(err)),
            }
        }
        () = &mut timeout_sleep => {
            let mut st = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let status = st.status.clone();
            let turn = st.turn.clone();
            st.finish("timeout", &status, turn);
            return Ok(ThreadWaitOutcome {
                outcome: "timeout".to_owned(),
                status,
                waited_ms: st.started.elapsed().as_millis() as u64,
                turn_id: st.turn.clone(),
            });
        }
    };

    let state_ended = state.clone();
    let ended_fut = watch.ended();
    tokio::spawn(async move {
        let end = ended_fut.await;
        let mut st = state_ended
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let outcome = if end == "session_closed" {
            "settled"
        } else {
            "ended"
        };
        let status = if end == "session_closed" {
            "closed"
        } else {
            &end
        };
        let turn = st.turn.clone();
        st.finish(outcome, status, turn);
    });

    let start_quiet = {
        let mut st = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !st.live
            && let Some(init) = watch.initial()
        {
            st.status = init.status.clone();
            if in_progress(&init.status) {
                st.run_seen(init.current_turn_id);
            }
        }
        !st.running
    };

    if start_quiet {
        let state_q = state.clone();
        let notify_q = quiet_notify.clone();
        tokio::spawn(async move {
            tokio::select! {
                () = tokio::time::sleep(Duration::from_millis(QUIET_MS)) => {
                    let mut st = state_q.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    if !st.running {
                        let status = st.status.clone();
                        st.finish("settled", &status, None);
                    }
                }
                () = notify_q.notified() => {}
            }
        });
    }

    let outcome = tokio::select! {
        res = &mut rx => {
            match res {
                Ok(out) => out,
                Err(_) => {
                    let st = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    ThreadWaitOutcome {
                        outcome: "ended".to_owned(),
                        status: st.status.clone(),
                        waited_ms: st.started.elapsed().as_millis() as u64,
                        turn_id: st.turn.clone(),
                    }
                }
            }
        }
        () = &mut timeout_sleep => {
            let mut st = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let status = st.status.clone();
            let turn = st.turn.clone();
            st.finish("timeout", &status, turn.clone());
            ThreadWaitOutcome {
                outcome: "timeout".to_owned(),
                status,
                waited_ms: st.started.elapsed().as_millis() as u64,
                turn_id: turn,
            }
        }
    };
    watch.stop();
    Ok(outcome)
}
