//! Job workers of a tool child (senpi `tool-child/gated-worker.ts`; senpi#3566, plan todo 4). Heavy jobs and offloaded
//! extension bodies run in workers so the protocol loop stays free; `start_gated_worker` starts one with the barrier
//! clock and routes its launch messages to the gate, and `install_worker_barrier` (first thing in the worker) gates
//! every process launch that worker makes.
//!
//! A worker is a dedicated OS thread running its own single-threaded runtime, so a launch can block it on the ack while
//! the protocol loop (the caller's runtime) stays free to receive the ack. `terminate` ends the worker's future at its
//! next await point (the closest native equivalent of `Worker.terminate()`: a thread blocked synchronously cannot be
//! interrupted).
use std::future::Future;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use tokio::sync::{mpsc, oneshot, watch};

use crate::barrier_clock::BarrierClock;
use crate::gated_bun::{GatedBun, install_gated_bun};
use crate::gated_child_process::{GatedChildProcess, SharedClient, create_gated_child_process};
use crate::gated_modules::{NestedWorkerRefusedError, refused_worker};
use crate::launch_env_names::{ProcessEnv, tool_child_reentry};
use crate::launch_gate::LaunchGate;
use crate::test_controls::{TestSwitch, read_test_controls};
use crate::worker_launch::{WorkerLaunchClient, WorkerLaunchClientOptions, WorkerLaunchMessage};

/// `GatedWorkerData`: what a started worker is given.
#[derive(Clone)]
pub struct GatedWorkerData {
    pub barrier_clock: BarrierClock,
    pub job_id: String,
    /// The tool child's re-entry command: scrubbed from every environment, so it travels in the worker data.
    pub reentry: Option<String>,
}

/// What `install_worker_barrier` gives a worker: the client and every gated launch API built on it.
#[derive(Clone)]
pub struct WorkerBarrier {
    pub client: SharedClient,
    pub child_process: GatedChildProcess,
    pub bun: GatedBun,
    pub job_id: String,
}

impl WorkerBarrier {
    /// A job worker never starts threads of its own: their launches would not pass this barrier.
    pub fn start_thread(&self) -> Result<std::convert::Infallible, NestedWorkerRefusedError> {
        refused_worker()
    }
}

/// `installWorkerBarrier(data, env)`: builds the worker's launch client (posting through `post`) and gates both launch
/// APIs on it. There is no builtin module to patch in place; the worker uses the returned gated APIs.
pub fn install_worker_barrier(
    data: &GatedWorkerData,
    post: Box<dyn Fn(WorkerLaunchMessage) + Send + Sync>,
    env: &ProcessEnv,
) -> WorkerBarrier {
    let job_id = data.job_id.clone();
    let id_for_client = job_id.clone();
    let client: SharedClient = Arc::new(Mutex::new(WorkerLaunchClient::new(
        WorkerLaunchClientOptions {
            clock: data.barrier_clock.clone(),
            post,
            job_id: Box::new(move || id_for_client.clone()),
            skip_ack_wait: read_test_controls(env).enabled(TestSwitch::SkipAckWait),
            ack_timeout_ms: None,
            worker_id: None,
        },
    )));
    let win32 = cfg!(windows);
    WorkerBarrier {
        child_process: create_gated_child_process(Arc::clone(&client), win32),
        bun: install_gated_bun(Arc::clone(&client), !win32),
        client,
        job_id,
    }
}

/// A started job worker.
pub struct GatedWorker {
    terminate: Mutex<Option<oneshot::Sender<()>>>,
    finished: watch::Receiver<bool>,
    terminated: AtomicBool,
}

impl GatedWorker {
    /// `worker.terminate()`: ends the worker's future; settle with [`GatedWorker::terminated`].
    pub fn terminate(&self) {
        let sender = self
            .terminate
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(sender) = sender {
            self.terminated.store(true, Ordering::SeqCst);
            let _ = sender.send(());
        }
    }

    /// True once `terminate` ended it.
    pub fn was_terminated(&self) -> bool {
        self.terminated.load(Ordering::SeqCst)
    }

    /// Resolves when the worker's thread has ended.
    pub async fn finished(&self) {
        let mut finished = self.finished.clone();
        let _ = finished.wait_for(|done| *done).await;
    }
}

/// Marks the worker finished however its thread ends (a panic included).
struct FinishGuard(watch::Sender<bool>);

impl Drop for FinishGuard {
    fn drop(&mut self) {
        self.0.send_replace(true);
    }
}

/// `startGatedWorker` with an explicit route for the worker's launch messages. Must be called inside a tokio runtime:
/// the messages are delivered in order from a task of that runtime (the protocol loop), never from the worker thread.
pub fn start_gated_worker_with<F, Fut>(
    route: Arc<dyn Fn(WorkerLaunchMessage) + Send + Sync>,
    barrier_clock: BarrierClock,
    job_id: &str,
    env: ProcessEnv,
    run: F,
) -> std::io::Result<GatedWorker>
where
    F: FnOnce(WorkerBarrier) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + 'static,
{
    let data = GatedWorkerData {
        barrier_clock,
        job_id: job_id.to_string(),
        reentry: tool_child_reentry(),
    };
    let (post_tx, mut post_rx) = mpsc::unbounded_channel::<WorkerLaunchMessage>();
    tokio::spawn(async move {
        while let Some(message) = post_rx.recv().await {
            route(message);
        }
    });
    let (terminate_tx, terminate_rx) = oneshot::channel::<()>();
    let (finished_tx, finished) = watch::channel(false);
    std::thread::Builder::new()
        .name(format!("tool-child-worker-{job_id}"))
        .spawn(move || {
            let _finished = FinishGuard(finished_tx);
            let post: Box<dyn Fn(WorkerLaunchMessage) + Send + Sync> = Box::new(move |message| {
                let _ = post_tx.send(message);
            });
            let barrier = install_worker_barrier(&data, post, &env);
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            runtime.block_on(async move {
                tokio::select! {
                    () = run(barrier) => {}
                    _ = terminate_rx => {}
                }
            });
            runtime.shutdown_background();
        })?;
    Ok(GatedWorker {
        terminate: Mutex::new(Some(terminate_tx)),
        finished,
        terminated: AtomicBool::new(false),
    })
}

/// `startGatedWorker(gate, file, jobId, options)`: a worker whose launch messages reach `gate`.
pub fn start_gated_worker<F, Fut>(
    gate: &Arc<LaunchGate>,
    job_id: &str,
    env: ProcessEnv,
    run: F,
) -> std::io::Result<GatedWorker>
where
    F: FnOnce(WorkerBarrier) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + 'static,
{
    let target = Arc::clone(gate);
    start_gated_worker_with(
        Arc::new(move |message| target.handle_worker(message)),
        gate.clock.clone(),
        job_id,
        env,
        run,
    )
}

/// A worker entry beside the calling module, named by its source path: the built package ships `.js`, so the extension
/// follows the caller's own file. Returns the resolved URL.
pub fn sibling_worker_url(source_specifier: &str, caller_url: &str) -> String {
    let caller_path = caller_url.split(['?', '#']).next().unwrap_or(caller_url);
    let extension = Path::new(caller_path)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| format!(".{ext}"));
    let specifier = match (source_specifier.strip_suffix(".ts"), extension) {
        (Some(stem), Some(extension)) => format!("{stem}{extension}"),
        _ => source_specifier.to_string(),
    };
    let base = caller_path
        .rfind('/')
        .map_or("", |index| &caller_path[..=index]);
    let mut resolved = base.to_string();
    let mut rest = specifier.as_str();
    loop {
        if let Some(next) = rest.strip_prefix("./") {
            rest = next;
        } else if let Some(next) = rest.strip_prefix("../") {
            let trimmed = resolved.trim_end_matches('/');
            resolved = trimmed
                .rfind('/')
                .map_or(String::new(), |index| trimmed[..=index].to_string());
            rest = next;
        } else {
            break;
        }
    }
    resolved.push_str(rest);
    resolved
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::gated_child_process::SpawnOptions;
    #[cfg(unix)]
    use crate::launch_barrier::StdioKind;
    use std::time::Duration;

    const BOUND: Duration = Duration::from_secs(30);

    #[test]
    fn a_worker_entry_uses_the_callers_own_extension() {
        assert_eq!(
            sibling_worker_url(
                "./module-job-worker.ts",
                "file:///pkg/dist/modes/rpc/tool-child/module-job.js"
            ),
            "file:///pkg/dist/modes/rpc/tool-child/module-job-worker.js"
        );
        assert_eq!(
            sibling_worker_url(
                "./module-job-worker.ts",
                "file:///pkg/src/modes/rpc/tool-child/module-job.ts"
            ),
            "file:///pkg/src/modes/rpc/tool-child/module-job-worker.ts"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn forwards_a_workers_started_report_to_the_route_as_started() {
        let (seen_tx, mut seen) = mpsc::unbounded_channel();
        let route: Arc<dyn Fn(WorkerLaunchMessage) + Send + Sync> = Arc::new(move |message| {
            let _ = seen_tx.send(message);
        });
        let env: ProcessEnv = [(
            "SENPI_TOOL_CHILD_TEST_SKIP_ACK_WAIT".to_string(),
            "1".to_string(),
        )]
        .into();
        let clock = BarrierClock::create(4).expect("clock");
        let worker = start_gated_worker_with(route, clock, "j1", env, |barrier| async move {
            // Windows mode (job containment) on every OS, as the upstream fixture does.
            let win32 = create_gated_child_process(Arc::clone(&barrier.client), true);
            let options = SpawnOptions {
                stdio: [StdioKind::Ignore; 3],
                detached: Some(false),
                ..SpawnOptions::default()
            };
            if let Ok(child) = win32.spawn("true", &[], &options) {
                child.wait();
            }
        })
        .expect("worker");
        let started = tokio::time::timeout(BOUND, async {
            while let Some(message) = seen.recv().await {
                if let WorkerLaunchMessage::Started { pid, breakaway, .. } = message {
                    return Some((pid, breakaway));
                }
            }
            None
        })
        .await
        .expect("started in time");
        let (pid, breakaway) = started.expect("a started report");
        assert!(pid > 0);
        assert!(!breakaway);
        tokio::time::timeout(BOUND, worker.finished())
            .await
            .expect("worker ends");
    }

    #[tokio::test]
    async fn terminate_ends_a_worker_that_never_settles() {
        let clock = BarrierClock::create(1).expect("clock");
        let worker = start_gated_worker_with(
            Arc::new(|_| {}),
            clock,
            "j2",
            ProcessEnv::new(),
            |_barrier| std::future::pending::<()>(),
        )
        .expect("worker");
        worker.terminate();
        tokio::time::timeout(BOUND, worker.finished())
            .await
            .expect("worker ends");
        assert!(worker.was_terminated());
    }
}
