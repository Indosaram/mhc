//! Background session announcement and opt-in auto-reap (`senpi#6201`).
//!
//! A session that runs on in the background (`run_in_background`, or a foreground command that
//! auto-detached) is announced to the host and, only when opted into, stopped at a configured age.
//! The reap runs where the session's process group lives (this process, or the tool child),
//! through `crate::manager::stop_shared`: the same TERM-then-KILL stop `kill_bash` uses.
//! Its one timer is cancelled when the session exits; with no opt-in none is armed.
//! A reap counts only when that stop resolved and the session really exited; a stop that fails or
//! leaves the session running is reported once (`on_background_reap_failed`) and never retried.

use crate::manager::{TerminalManager, stop_shared};
use crate::runtime_session::TerminalRuntimeSession;
use crate::tools::context::{TerminalExitView, TerminalToolContext};
use serde_json::json;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const MINUTE_MS: u64 = 60_000;

/// The call's `auto_reap_minutes` wins over the setting; a whole number of minutes >= 1 reaps,
/// 0 or less turns it off.
pub fn resolve_auto_reap_minutes(requested: Option<f64>, configured: Option<u64>) -> Option<u64> {
    if let Some(value) = requested {
        if !value.is_finite() {
            return None;
        }
        let minutes = value.trunc();
        return if minutes >= 1.0 {
            Some(minutes as u64)
        } else {
            None
        };
    }
    configured.filter(|minutes| *minutes >= 1)
}

#[derive(Clone, Debug, PartialEq)]
pub struct BackgroundSessionStart {
    pub description: String,
    pub command: String,
    pub started_at_ms: f64,
    pub auto_reap_minutes: Option<u64>,
}

pub fn background_start(
    command: &str,
    description: Option<&str>,
    auto_reap_minutes: Option<f64>,
    configured_minutes: Option<u64>,
    started_at_ms: f64,
) -> BackgroundSessionStart {
    BackgroundSessionStart {
        description: description.unwrap_or(command).to_owned(),
        command: command.to_owned(),
        started_at_ms,
        auto_reap_minutes: resolve_auto_reap_minutes(auto_reap_minutes, configured_minutes),
    }
}

fn warn_reap_failed(ctx: &TerminalToolContext, id: &str, age: u64, error: &str) {
    if let Some(logger) = &ctx.session_logger {
        logger(
            "bash_auto_reap_failed",
            json!({
                "bash_id": id,
                "autoReapMinutes": age,
                "error": error,
            }),
        );
    } else {
        maho_ext_api::diagnostic(&format!(
            "bash_auto_reap_failed: bash_id={id} autoReapMinutes={age} error={error}"
        ));
    }
}

fn current_epoch_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64() * 1000.0)
        .unwrap_or(0.0)
}

/// Announce `id` as a background session and arm the exit watcher + optional auto-reap timer.
///
/// Calls `ctx.on_background_start` with no manager lock held (callers may lock `manager` inside the
/// hook), then reads the runtime handles from `manager`.
pub fn track_background_session(
    ctx: &TerminalToolContext,
    manager: Arc<Mutex<TerminalManager>>,
    id: String,
    start: BackgroundSessionStart,
) {
    if let Some(on_start) = &ctx.on_background_start {
        on_start(
            &id,
            &start.description,
            start.started_at_ms,
            Some(&start.command),
            start.auto_reap_minutes,
        );
    }
    let minutes = start.auto_reap_minutes;
    if minutes.is_none() && ctx.on_background_exit.is_none() {
        return;
    }
    let (completion, already_exited, disposed, dispose_rx) = {
        let mut guard = manager
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(runtime) = guard.get(&id) else {
            return;
        };
        setup_runtime_handles(runtime)
    };
    spawn_background_tracker(
        ctx.clone(),
        manager,
        id,
        start,
        completion,
        already_exited,
        disposed,
        dispose_rx,
        current_epoch_ms,
    );
}

#[cfg_attr(not(test), allow(dead_code))]
fn track_background_session_with_clock(
    ctx: &TerminalToolContext,
    manager: Arc<Mutex<TerminalManager>>,
    id: String,
    runtime: &TerminalRuntimeSession,
    start: BackgroundSessionStart,
    now_ms: fn() -> f64,
) {
    if let Some(on_start) = &ctx.on_background_start {
        on_start(
            &id,
            &start.description,
            start.started_at_ms,
            Some(&start.command),
            start.auto_reap_minutes,
        );
    }
    let minutes = start.auto_reap_minutes;
    if minutes.is_none() && ctx.on_background_exit.is_none() {
        return;
    }
    let (completion, already_exited, disposed, dispose_rx) = setup_runtime_handles(runtime);
    spawn_background_tracker(
        ctx.clone(),
        manager,
        id,
        start,
        completion,
        already_exited,
        disposed,
        dispose_rx,
        now_ms,
    );
}

fn setup_runtime_handles(
    runtime: &TerminalRuntimeSession,
) -> (
    crate::runtime_session::TerminalCompletionHandle,
    bool,
    Arc<AtomicBool>,
    tokio::sync::oneshot::Receiver<()>,
) {
    let completion = runtime.completion_handle();
    let exit_rx = completion.exit_receiver();
    let already_exited = runtime.exited().unwrap_or(false) || exit_rx.borrow().is_some();
    let disposed = Arc::new(AtomicBool::new(false));
    let disposed_flag = Arc::clone(&disposed);
    let (dispose_tx, dispose_rx) = tokio::sync::oneshot::channel::<()>();
    let dispose_tx = Mutex::new(Some(dispose_tx));
    let _dispose_listener_id = runtime.on_dispose(move || {
        disposed_flag.store(true, Ordering::SeqCst);
        if let Ok(mut guard) = dispose_tx.lock()
            && let Some(tx) = guard.take()
        {
            let _ = tx.send(());
        }
    });
    (completion, already_exited, disposed, dispose_rx)
}

#[allow(clippy::too_many_arguments)]
fn spawn_background_tracker(
    ctx: TerminalToolContext,
    manager: Arc<Mutex<TerminalManager>>,
    id: String,
    start: BackgroundSessionStart,
    completion: crate::runtime_session::TerminalCompletionHandle,
    already_exited: bool,
    disposed: Arc<AtomicBool>,
    mut dispose_rx: tokio::sync::oneshot::Receiver<()>,
    now_ms: fn() -> f64,
) {
    let minutes = start.auto_reap_minutes;
    let mut exit_rx = completion.exit_receiver();
    let deadline_ms = minutes
        .filter(|_| !already_exited)
        .map(|age| start.started_at_ms + (age.saturating_mul(MINUTE_MS)) as f64);

    tokio::spawn(async move {
        let mut reaped_after_minutes: Option<u64> = None;

        let reap_duration = deadline_ms.map(|deadline| {
            let remaining_ms = (deadline - now_ms()).max(0.0);
            Duration::from_millis(remaining_ms.ceil() as u64)
        });
        let sleep = async {
            if let Some(delay) = reap_duration {
                tokio::time::sleep(delay).await;
            } else {
                std::future::pending::<()>().await;
            }
        };
        tokio::pin!(sleep);

        loop {
            if exit_rx.borrow_and_update().is_some() {
                break;
            }
            tokio::select! {
                changed = exit_rx.changed() => {
                    if changed.is_err() || exit_rx.borrow().is_some() {
                        break;
                    }
                }
                _ = &mut sleep, if reap_duration.is_some() => {
                    let Some(age) = minutes else { break; };
                    if exit_rx.borrow().is_some() {
                        break;
                    }
                    let failure: Option<String> = match stop_shared(&manager, &id).await {
                        Ok(true) => {
                            let exited = exit_rx.borrow_and_update().is_some()
                                || manager
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                                    .get(&id)
                                    .is_none_or(|rt| rt.exited().unwrap_or(false));
                            if exited {
                                None
                            } else {
                                Some("the session was still running after the stop (TERM, then KILL)".to_owned())
                            }
                        }
                        Ok(false) => Some(format!("the terminal manager has no session {id} to stop")),
                        Err(error) => Some(error.to_string()),
                    };
                    if let Some(reason) = failure {
                        warn_reap_failed(&ctx, &id, age, &reason);
                        let still_running = exit_rx.borrow().is_none() && !disposed.load(Ordering::SeqCst);
                        if still_running && let Some(cb) = &ctx.on_background_reap_failed {
                            cb(&id, age, &reason);
                        }
                        // Never retry; keep waiting for natural exit or disposal.
                        loop {
                            if exit_rx.borrow_and_update().is_some() {
                                break;
                            }
                            tokio::select! {
                                changed = exit_rx.changed() => {
                                    if changed.is_err() || exit_rx.borrow().is_some() {
                                        break;
                                    }
                                }
                                _ = &mut dispose_rx => {
                                    break;
                                }
                            }
                        }
                    } else {
                        reaped_after_minutes = Some(age);
                    }
                    break;
                }
                _ = &mut dispose_rx => {
                    break;
                }
            }
        }

        if (exit_rx.borrow().is_some() || disposed.load(Ordering::SeqCst))
            && let Some(on_exit) = &ctx.on_background_exit
        {
            let mut view = TerminalExitView::of_completion(&completion);
            if let Some(age) = reaped_after_minutes {
                view = view.with_auto_reaped_after_minutes(age);
            }
            on_exit(&id, view);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::TERMINAL_SETTINGS_DEFAULTS;
    use std::collections::BTreeMap;

    fn test_ctx() -> TerminalToolContext {
        let config = Arc::new(Mutex::new((TERMINAL_SETTINGS_DEFAULTS, None)));
        TerminalToolContext::new(config, Arc::new(BTreeMap::new))
    }

    #[test]
    fn resolve_auto_reap_minutes_call_overrides_setting_and_zero_disables() {
        assert_eq!(resolve_auto_reap_minutes(None, None), None);
        assert_eq!(resolve_auto_reap_minutes(None, Some(15)), Some(15));
        assert_eq!(resolve_auto_reap_minutes(Some(0.0), Some(15)), None);
        assert_eq!(resolve_auto_reap_minutes(Some(-2.0), Some(15)), None);
        assert_eq!(resolve_auto_reap_minutes(Some(3.9), Some(15)), Some(3));
        assert_eq!(resolve_auto_reap_minutes(Some(f64::NAN), Some(15)), None);
        let started = background_start("sleep 60", None, Some(2.0), Some(10), 1_000.0);
        assert_eq!(started.description, "sleep 60");
        assert_eq!(started.auto_reap_minutes, Some(2));
    }

    #[tokio::test(start_paused = true)]
    async fn track_background_session_auto_reaps_at_configured_minutes() {
        let mut ctx = test_ctx();
        let (exit_tx, mut exit_rx) = tokio::sync::mpsc::unbounded_channel();
        ctx.on_background_exit = Some(Arc::new(move |id, view| {
            let _ = exit_tx.send((id.to_owned(), view.auto_reaped_after_minutes));
        }));

        let manager = Arc::new(Mutex::new(TerminalManager::default()));
        let id = manager
            .lock()
            .expect("manager")
            .create("sleep 60", crate::shared::test_sh_options("sleep 60"))
            .expect("spawn");
        {
            let mut mgr = manager.lock().expect("manager");
            let runtime = mgr.get(&id).expect("runtime");
            let start = background_start("sleep 60", Some("sleeper"), Some(2.0), None, 0.0);
            track_background_session_with_clock(
                &ctx,
                Arc::clone(&manager),
                id.clone(),
                runtime,
                start,
                || 0.0,
            );
        }

        tokio::task::yield_now().await;
        assert!(exit_rx.try_recv().is_err());

        tokio::time::advance(Duration::from_millis(120_000)).await;
        tokio::time::resume();
        let (exited_id, reaped_after) =
            tokio::time::timeout(Duration::from_secs(5), exit_rx.recv())
                .await
                .expect("timeout")
                .expect("exit event");
        assert_eq!(exited_id, id);
        assert_eq!(reaped_after, Some(2));
        crate::manager::teardown_shared(&manager)
            .await
            .expect("teardown");
    }

    #[tokio::test(start_paused = true)]
    async fn track_background_session_reports_failed_reap_once_without_retry() {
        let mut ctx = test_ctx();
        let (fail_tx, mut fail_rx) = tokio::sync::mpsc::unbounded_channel();
        ctx.on_background_reap_failed = Some(Arc::new(move |id, age, reason| {
            let _ = fail_tx.send((id.to_owned(), age, reason.to_owned()));
        }));

        // Deliberately track with a missing id so `stop_shared` returns Ok(false).
        let manager = Arc::new(Mutex::new(TerminalManager::default()));
        let runtime =
            TerminalRuntimeSession::start("sleep 60", crate::shared::test_sh_options("sleep 60"))
                .expect("spawn");
        let start = background_start("sleep 60", Some("orphan"), Some(1.0), None, 0.0);
        track_background_session_with_clock(
            &ctx,
            Arc::clone(&manager),
            "bash_missing".to_owned(),
            &runtime,
            start,
            || 0.0,
        );

        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(60_000)).await;
        tokio::task::yield_now().await;
        let (failed_id, age, reason) = fail_rx.try_recv().expect("fail event");
        assert_eq!(failed_id, "bash_missing");
        assert_eq!(age, 1);
        assert!(reason.contains("bash_missing"));

        tokio::time::advance(Duration::from_millis(120_000)).await;
        tokio::task::yield_now().await;
        assert!(fail_rx.try_recv().is_err());

        tokio::time::resume();
        runtime.dispose().expect("dispose orphan");
    }
}
