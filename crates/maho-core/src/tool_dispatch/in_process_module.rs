//! `ctx.offload` under the in-process runtime (senpi#3622): the registered module runs in this process with the same
//! host the tool child's worker builds. Code here cannot be stopped, so a module past its deadline keeps its locks and,
//! after a grace, is reported stuck; closing the session closes its runs.
use super::in_process_host_calls::{in_process_host_calls, settle_now};
use super::offload::{BoxError, OffloadError, OffloadErrorCode};
use super::tool_dispatcher::ToolChildModuleJob;
use futures::{FutureExt, future::BoxFuture};
use maho_ext_api::AbortSignal;
#[cfg(test)]
use maho_tool_child::module_job_worker::register_child_module;
use maho_tool_child::module_job_worker::{ModuleExport, load_module_export};
use maho_tool_child::{host_calls_server::HostCallHandler, job_policy::OFFLOAD_MAX_MS};
use serde_json::Value;
use std::{
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::{oneshot, watch};

/// The live runs; the dispatcher ends them all (cancelled, host calls refused, locks released) when the session is closed or replaced.
pub type InProcessLive = Arc<Mutex<Vec<Arc<InProcessRun>>>>;

/// What `InProcessToolDispatcher` gives every in-process module run.
#[derive(Clone, Default)]
pub struct InProcessModuleRuntime {
    /// The session's host-call handler; unset, every host call fails `tool_runtime_unavailable`.
    pub host_calls: Option<HostCallHandler>,
    /// How long past its deadline a module may keep running before it is reported stuck (default 10 s).
    pub stuck_grace_ms: Option<u64>,
    /// Told when a module is stuck (past its deadline and grace: it keeps its locks, and every waiter on them fails fast),
    /// or still running a grace after its session closed (its locks were released with the session).
    pub on_stuck: Option<Arc<dyn Fn(StuckModuleNotice) + Send + Sync>>,
    pub live: Option<InProcessLive>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StuckModuleNotice {
    pub module_path: String,
    pub export: String,
    pub deadline_ms: u64,
    /// The files whose locks it still holds (as the module named them).
    pub files: Vec<String>,
    /// What to show the user.
    pub notice: String,
}

type CloseFn = Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>;

/// A live in-process run, as the dispatcher sees it.
pub struct InProcessRun {
    close: Mutex<CloseFn>,
}

impl InProcessRun {
    /// The session is closed or replaced. A run still loading its module never starts its body. A running module is
    /// cancelled and every further host call it makes is refused (`host_call_session_closed`); this settles once its locks
    /// are released - when its body settles, at once if it is already stuck, or after the grace (then reported).
    pub async fn close(&self) {
        let close = lock(&self.close).clone();
        close().await;
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

const DEFAULT_STUCK_GRACE_MS: u64 = 10_000;

fn forget(live: &Option<InProcessLive>, run: &Arc<InProcessRun>) {
    if let Some(live) = live {
        lock(live).retain(|entry| !Arc::ptr_eq(entry, run));
    }
}

/// Looks the module's export up in the shared registry, unless the session closed first: then nothing of it runs.
fn load_export(
    job: &ToolChildModuleJob,
    closed: &AbortSignal,
) -> Result<ModuleExport, OffloadError> {
    let closed_error = || {
        OffloadError::new(
            OffloadErrorCode::OffloadSessionClosed,
            format!("{}: the session was closed", job.module_path),
        )
    };
    if closed.is_aborted() {
        return Err(closed_error());
    }
    // The single module registry the tool child's job worker reads (`register_child_module`).
    let target = load_module_export(&job.module_path, &job.export);
    // The load can finish in the same turn the session closes: the body never starts after a close.
    if closed.is_aborted() {
        return Err(closed_error());
    }
    target.ok_or_else(|| {
        OffloadError::new(
            OffloadErrorCode::OffloadModuleUnavailable,
            format!(
                "{} has no function export \"{}\"",
                job.module_path, job.export
            ),
        )
    })
}

/// The in-process path: loads the module and runs its export with `(args, host)`, awaited - the same host the tool
/// child's worker builds, its calls answered by the session's host-call handler. Its deadline is the child's (the
/// caller's value, at most `OFFLOAD_MAX_MS`). Code in this process cannot be stopped, so past the deadline the module
/// keeps its locks - releasing one could let a second writer interleave with a write still in flight - and after a grace
/// it is reported stuck: every waiter on its locks fails fast, until it finishes or the session is closed. The run is
/// registered with the session before its module loads, so a close during the load leaves the body unstarted.
///
/// # Errors
/// An [`OffloadError`] (`offload_session_closed`, `offload_module_unavailable`, `tool_child_deadline`), or the module's own failure.
pub async fn run_module_in_process(
    job: ToolChildModuleJob,
    signal: Option<AbortSignal>,
    runtime: InProcessModuleRuntime,
) -> Result<Value, BoxError> {
    let loading = AbortSignal::default();
    let close_loading: CloseFn = {
        let loading = loading.clone();
        Arc::new(move || {
            loading.abort();
            async {}.boxed()
        })
    };
    let registered = Arc::new(InProcessRun {
        close: Mutex::new(close_loading),
    });
    if let Some(live) = &runtime.live {
        lock(live).push(registered.clone());
    }
    let target = match load_export(&job, &loading) {
        Ok(target) => target,
        Err(error) => {
            forget(&runtime.live, &registered);
            return Err(error.into());
        }
    };
    let cancellation = AbortSignal::default();
    let forwarder = signal.map(|signal| {
        let cancellation = cancellation.clone();
        tokio::spawn(async move {
            signal.cancelled().await;
            cancellation.abort();
        })
    });
    let stop_forwarding = || {
        if let Some(forwarder) = &forwarder {
            forwarder.abort();
        }
    };
    let calls = Arc::new(in_process_host_calls(
        &job,
        cancellation.clone(),
        runtime.host_calls.clone(),
    ));
    // A close in the microtask between the load and here: checked in the same synchronous step that starts the body.
    if loading.is_aborted() {
        stop_forwarding();
        forget(&runtime.live, &registered);
        return Err(OffloadError::new(
            OffloadErrorCode::OffloadSessionClosed,
            format!("{}: the session was closed", job.module_path),
        )
        .into());
    }
    let run = settle_now(|| target(job.args.clone(), calls.host.clone()));
    let (body_settled, body_settled_rx) = watch::channel(false);
    let (result_tx, result_rx) = oneshot::channel::<Result<Value, BoxError>>();
    // The job ends when the module's own body settles - never at the deadline: a lock the body still holds (its write
    // still running) is released only then, as on the child, so no second writer can interleave with it.
    {
        let (calls, live, registered) = (calls.clone(), runtime.live.clone(), registered.clone());
        tokio::spawn(async move {
            let outcome = run.await;
            calls.end();
            forget(&live, &registered);
            body_settled.send_replace(true);
            let _ = result_tx.send(outcome);
        });
    }
    let holder = format!("{}#{}", job.module_path, job.export);
    let deadline_ms = job
        .deadline_ms
        .unwrap_or(OFFLOAD_MAX_MS)
        .min(OFFLOAD_MAX_MS) as u64;
    let grace_ms = runtime.stuck_grace_ms.unwrap_or(DEFAULT_STUCK_GRACE_MS);
    let child_hint = "Run untrusted or long-running modules with --tool-runtime child, where a stuck module is stopped.";
    let stuck = Arc::new(AtomicBool::new(false));
    type StuckReport = dyn Fn(&dyn Fn(&[String]) -> String) + Send + Sync;
    let report: Arc<StuckReport> = {
        let (calls, on_stuck, job) = (calls.clone(), runtime.on_stuck.clone(), job.clone());
        Arc::new(move |notice| {
            let held = calls.held_files();
            if let Some(on_stuck) = &on_stuck {
                on_stuck(StuckModuleNotice {
                    module_path: job.module_path.clone(),
                    export: job.export.clone(),
                    deadline_ms,
                    files: held.clone(),
                    notice: notice(&held),
                });
            }
        })
    };
    let mark_stuck: Arc<dyn Fn() + Send + Sync> = {
        let (calls, stuck, report, holder) =
            (calls.clone(), stuck.clone(), report.clone(), holder.clone());
        Arc::new(move || {
            if stuck.load(Ordering::SeqCst) || calls.ended() {
                return;
            }
            stuck.store(true, Ordering::SeqCst);
            calls.mark_stuck(&holder);
            report(&|held: &[String]| {
                format!(
                    "The module {holder} is stuck: it ran past its {deadline_ms} ms deadline and is still running in this process, where it cannot be stopped.{} {child_hint}",
                    if held.is_empty() {
                        String::new()
                    } else {
                        format!(
                            " Its lock on {} stays held, and every write waiting on {} now fails until it finishes or the session is closed.",
                            held.join(", "),
                            if held.len() == 1 {
                                "that file"
                            } else {
                                "those files"
                            }
                        )
                    }
                )
            });
        })
    };
    let close_started: CloseFn = {
        let (calls, cancellation, stuck, report, holder, live, registered) = (
            calls.clone(),
            cancellation.clone(),
            stuck.clone(),
            report.clone(),
            holder.clone(),
            runtime.live.clone(),
            registered.clone(),
        );
        Arc::new(move || {
            let (calls, cancellation, stuck, report, holder, live, registered) = (
                calls.clone(),
                cancellation.clone(),
                stuck.clone(),
                report.clone(),
                holder.clone(),
                live.clone(),
                registered.clone(),
            );
            let mut settled = body_settled_rx.clone();
            async move {
                calls.close();
                cancellation.abort();
                if !stuck.load(Ordering::SeqCst) {
                    let grace_over = tokio::time::sleep(Duration::from_millis(grace_ms));
                    let timed_out = tokio::select! {
                        _ = settled.wait_for(|done| *done) => false,
                        () = grace_over => true,
                    };
                    if timed_out && !calls.ended() {
                        report(&|held: &[String]| {
                            format!(
                                "The module {holder} did not finish within {grace_ms} ms of its session closing and is still running in this process, where it cannot be stopped.{} {child_hint}",
                                if held.is_empty() {
                                    String::new()
                                } else {
                                    format!(" Its lock on {} was released with the session.", held.join(", "))
                                }
                            )
                        });
                    }
                }
                // Settled, stuck, or out of grace with its session closed: nothing is left to keep its locks for.
                calls.end();
                forget(&live, &registered);
            }
            .boxed()
        })
    };
    *lock(&registered.close) = close_started;
    let outcome = tokio::select! {
        outcome = result_rx => outcome.unwrap_or_else(|_| Err(format!("{holder}: the module panicked").into())),
        () = tokio::time::sleep(Duration::from_millis(deadline_ms)) => {
            calls.stop_admitting();
            cancellation.abort();
            let files = calls.held_files();
            let grace = mark_stuck.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(grace_ms)).await;
                grace();
            });
            Err(OffloadError::new(
                OffloadErrorCode::ToolChildDeadline,
                format!(
                    "{}#{} ran past {deadline_ms} ms{}",
                    job.module_path,
                    job.export,
                    if files.is_empty() { String::new() } else { format!("; it still holds the lock on {} until it finishes", files.join(", ")) }
                ),
            ).into())
        }
    };
    stop_forwarding();
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const BOUND: Duration = Duration::from_secs(5);

    fn job(path: &str, deadline_ms: Option<f64>) -> ToolChildModuleJob {
        ToolChildModuleJob {
            module_path: path.into(),
            export: "run".into(),
            args: json!({ "toolCallId": "call-1" }),
            idempotent: None,
            deadline_ms,
        }
    }

    /// Registers `export` in the shared registry under a path unique to the test (the registry is process-wide).
    fn registered(path: &str, export: ModuleExport) -> InProcessModuleRuntime {
        register_child_module(path, "run", export);
        InProcessModuleRuntime::default()
    }

    #[tokio::test]
    async fn runs_the_export_with_its_args_and_returns_its_value() {
        let export: ModuleExport =
            Arc::new(|args, _host| async move { Ok(json!({ "echo": args })) }.boxed());
        let path = "/in-process-module-test/echo.js";
        let value = tokio::time::timeout(
            BOUND,
            run_module_in_process(job(path, None), None, registered(path, export)),
        )
        .await
        .expect("bounded")
        .expect("ran");
        assert_eq!(value, json!({ "echo": { "toolCallId": "call-1" } }));
    }

    #[tokio::test]
    async fn a_module_that_is_not_registered_is_unavailable() {
        let path = "/in-process-module-test/never-registered.js";
        let none =
            run_module_in_process(job(path, None), None, InProcessModuleRuntime::default()).await;
        assert_eq!(
            none.err().map(|error| error.to_string()).as_deref(),
            Some(
                "offload_module_unavailable: /in-process-module-test/never-registered.js has no function export \"run\""
            )
        );
    }

    #[tokio::test]
    async fn past_its_deadline_a_module_fails_tool_child_deadline_and_is_reported_stuck_after_the_grace()
     {
        let (release, gate) = oneshot::channel::<()>();
        let gate = Arc::new(Mutex::new(Some(gate)));
        let export: ModuleExport = Arc::new(move |_, _| {
            let gate = lock(&gate).take();
            async move {
                if let Some(gate) = gate {
                    let _ = gate.await;
                }
                Ok(Value::Null)
            }
            .boxed()
        });
        let path = "/in-process-module-test/stuck.js";
        let (notice_tx, mut notice_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut runtime = registered(path, export);
        runtime.stuck_grace_ms = Some(50);
        runtime.on_stuck = Some(Arc::new(move |notice| {
            let _ = notice_tx.send(notice);
        }));
        let outcome = run_module_in_process(job(path, Some(10.0)), None, runtime).await;
        assert_eq!(
            outcome.err().map(|error| error.to_string()).as_deref(),
            Some("tool_child_deadline: /in-process-module-test/stuck.js#run ran past 10 ms")
        );
        let notice = tokio::time::timeout(BOUND, notice_rx.recv())
            .await
            .expect("bounded")
            .expect("notice");
        assert_eq!(notice.deadline_ms, 10);
        assert!(notice.notice.starts_with("The module /in-process-module-test/stuck.js#run is stuck: it ran past its 10 ms deadline"));
        drop(release);
    }
}
