//! senpi 903b8336 test/idle-parking-kernel.test.ts, on tokio's paused clock: each park is awaited
//! through the fake kernel's close signal, and "not parked" is checked after the clock ran past the
//! idle time with every task idle.

use maho_codemode::extension::idle_parking_kernel::*;
use maho_codemode::tool::types::*;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::watch;

const MINUTE: Duration = Duration::from_secs(60);

struct FakeKernel {
    id: usize,
    ran: Mutex<Vec<String>>,
    closed: watch::Sender<bool>,
    queued: Mutex<Vec<String>>,
    tools: Mutex<Vec<String>>,
    /// When set, `run` waits for this gate to open before settling.
    run_gate: Mutex<Option<watch::Receiver<bool>>>,
    /// When set, `close` waits for this gate to open before the kernel counts as closed.
    close_gate: Mutex<Option<watch::Receiver<bool>>>,
    /// When set, the kernel exposes kernel tools whose `invoke` waits for this gate.
    tool_gate: Mutex<Option<watch::Receiver<bool>>>,
    described: Mutex<Vec<Vec<String>>>,
}

impl FakeKernel {
    fn new(id: usize) -> Arc<Self> {
        Arc::new(Self {
            id,
            ran: Mutex::new(vec![]),
            closed: watch::channel(false).0,
            queued: Mutex::new(vec![]),
            tools: Mutex::new(vec![]),
            run_gate: Mutex::new(None),
            close_gate: Mutex::new(None),
            tool_gate: Mutex::new(None),
            described: Mutex::new(vec![]),
        })
    }
    fn is_closed(&self) -> bool {
        *self.closed.borrow()
    }
    async fn wait_closed(&self) {
        let mut closed = self.closed.subscribe();
        tokio::time::timeout(10 * MINUTE, closed.wait_for(|closed| *closed))
            .await
            .expect("kernel parked")
            .expect("close signal");
    }
    fn ran(&self) -> Vec<String> {
        self.ran.lock().unwrap().clone()
    }
}

struct FakeTools(Arc<FakeKernel>);

impl maho_ext_api::ExtensionKernelTools for FakeTools {
    fn invoke_scope(&self) -> bool {
        true
    }
    fn describe<'a>(&'a self, names: &'a [String]) -> maho_ext_api::ExtensionFuture<'a, Value> {
        Box::pin(async move {
            self.0.described.lock().unwrap().push(names.to_vec());
            Ok(json!({"results":[]}))
        })
    }
    fn invoke(
        &self,
        _: maho_ext_api::KernelToolInvokeRequest,
        _: maho_ext_api::KernelToolInvokeOptions,
    ) -> maho_ext_api::ExtensionFuture<'_, Value> {
        Box::pin(async move {
            let gate = self.0.tool_gate.lock().unwrap().clone();
            if let Some(mut gate) = gate {
                let _ = gate.wait_for(|open| *open).await;
            }
            Ok(json!("done"))
        })
    }
}

impl EvalKernel for FakeKernel {
    fn kernel_tools(self: Arc<Self>) -> Option<Arc<dyn maho_ext_api::ExtensionKernelTools>> {
        if self.tool_gate.lock().unwrap().is_some() {
            Some(Arc::new(FakeTools(self)))
        } else {
            None
        }
    }
    fn run(&self, input: EvalKernelRunInput) -> EvalKernelFuture<'_, Value> {
        Box::pin(async move {
            if self.is_closed() {
                return Err(format!("kernel {} is closed", self.id));
            }
            self.ran.lock().unwrap().push(input.cell_id.clone());
            let gate = self.run_gate.lock().unwrap().clone();
            if let Some(mut gate) = gate {
                let _ = gate.wait_for(|open| *open).await;
            }
            Ok(
                json!({"type":"result","cellId":input.cell_id,"ok":true,"valueRepr":format!("kernel {}", self.id),"durationMs":1}),
            )
        })
    }
    fn cancel_queued<'a>(&'a self, _: &'a str, _: &'a str) -> EvalKernelFuture<'a, bool> {
        Box::pin(async { Ok(false) })
    }
    fn interrupt<'a>(
        &'a self,
        _: &'a str,
        _: Option<&'a str>,
    ) -> EvalKernelFuture<'a, KernelInterruptHandle> {
        Box::pin(async {
            Ok(KernelInterruptHandle {
                state_retained: Box::pin(async { Ok(true) }),
                note: None,
            })
        })
    }
    fn queue_snapshot(&self) -> (Option<String>, Vec<String>) {
        (None, self.queued.lock().unwrap().clone())
    }
    fn deliver_tool_reply(&self, _: Value) -> Result<(), String> {
        Ok(())
    }
    fn reset(&self) -> EvalKernelFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
    fn close(&self) -> EvalKernelFuture<'_, ()> {
        Box::pin(async {
            let gate = self.close_gate.lock().unwrap().clone();
            if let Some(mut gate) = gate {
                let _ = gate.wait_for(|open| *open).await;
            }
            self.closed.send_replace(true);
            Ok(())
        })
    }
    fn list_kernel_tool_names(&self) -> Vec<String> {
        self.tools.lock().unwrap().clone()
    }
}

/// Restarts hand out the given kernels in order (an `Err` entry fails that restart), counting calls.
fn restarts(
    kernels: Vec<Result<Arc<FakeKernel>, String>>,
) -> (RestartParkedKernel, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let queue = Arc::new(Mutex::new(kernels.into_iter()));
    let counter = calls.clone();
    let restart: RestartParkedKernel =
        Arc::new(move || -> EvalKernelFuture<'static, Arc<dyn EvalKernel>> {
            counter.fetch_add(1, Ordering::SeqCst);
            let next = queue.lock().unwrap().next().expect("restart configured");
            Box::pin(async move { next.map(|kernel| kernel as Arc<dyn EvalKernel>) })
        });
    (restart, calls)
}

fn cell(id: &str) -> EvalKernelRunInput {
    EvalKernelRunInput {
        cell_id: id.into(),
        code: "x".into(),
        ..Default::default()
    }
}

#[tokio::test(start_paused = true)]
async fn idle_kernel_is_parked_and_the_next_cell_runs_on_a_fresh_kernel_with_a_restart_notice() {
    let (first, second) = (FakeKernel::new(1), FakeKernel::new(2));
    let (restart, calls) = restarts(vec![Ok(second.clone())]);
    let parked = IdleParkingKernel::new(EvalLanguage::Js, 1.0, first.clone(), restart, None);

    first.wait_closed().await;
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    let result = parked.run(cell("after-park")).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(second.ran(), ["after-park"]);
    assert_eq!(result["ok"], true);
    assert_eq!(result["kernelState"], "restarted");
    let notice = result["notice"].as_str().unwrap();
    assert!(
        notice.contains("every global is lost") && notice.contains("memory.idleParkMinutes"),
        "{notice}"
    );

    let next = parked.run(cell("later")).await.unwrap();
    assert!(next.get("notice").is_none() && next.get("kernelState").is_none());
    parked.close().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_queued_cell_keeps_the_kernel_and_parking_follows_once_the_queue_drains() {
    let first = FakeKernel::new(1);
    *first.queued.lock().unwrap() = vec!["detached".into()];
    let (restart, _) = restarts(vec![]);
    let parked = IdleParkingKernel::new(EvalLanguage::Js, 1.0, first.clone(), restart, None);

    tokio::time::sleep(3 * MINUTE).await;
    assert!(!first.is_closed());
    first.queued.lock().unwrap().clear();
    first.wait_closed().await;
    parked.close().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_failed_restart_settles_that_cell_with_the_reason_and_the_next_cell_retries() {
    let (first, second) = (FakeKernel::new(1), FakeKernel::new(2));
    let (restart, calls) = restarts(vec![
        Err("interpreter not found".into()),
        Ok(second.clone()),
    ]);
    let parked = IdleParkingKernel::new(EvalLanguage::Js, 1.0, first.clone(), restart, None);
    first.wait_closed().await;

    let failed = parked.run(cell("first-try")).await.unwrap();
    assert_eq!(
        (failed["ok"].clone(), failed["kernelState"].clone()),
        (json!(false), json!("not-run"))
    );
    let message = failed["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("interpreter not found") && message.contains("run the cell again"),
        "{message}"
    );

    let retried = parked.run(cell("second-try")).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(retried["kernelState"], "restarted");
    assert_eq!(second.ran(), ["second-try"]);
    parked.close().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_closed_wrapper_keeps_no_timer_restarts_nothing_and_rejects_cells() {
    let first = FakeKernel::new(1);
    let (restart, calls) = restarts(vec![]);
    let parked = IdleParkingKernel::new(EvalLanguage::Js, 1.0, first.clone(), restart, None);

    parked.close().await.unwrap();
    assert!(first.is_closed());
    tokio::time::sleep(5 * MINUTE).await;
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(parked.run(cell("late")).await.unwrap_err(), "Kernel closed");
}

#[tokio::test(start_paused = true)]
async fn defined_tools_keep_the_kernel_until_they_are_gone() {
    let first = FakeKernel::new(1);
    *first.tools.lock().unwrap() = vec!["add".into()];
    let parked_count = Arc::new(AtomicUsize::new(0));
    let counter = parked_count.clone();
    let (restart, _) = restarts(vec![]);
    let parked = IdleParkingKernel::new(
        EvalLanguage::Js,
        1.0,
        first.clone(),
        restart,
        Some(Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        })),
    );

    tokio::time::sleep(10 * MINUTE).await;
    assert!(!first.is_closed());
    assert_eq!(parked_count.load(Ordering::SeqCst), 0);
    first.tools.lock().unwrap().clear();
    first.wait_closed().await;
    assert_eq!(parked_count.load(Ordering::SeqCst), 1);
    parked.close().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_kernel_without_kernel_tools_is_not_given_them() {
    let (restart, _) = restarts(vec![]);
    let parked = Arc::new(IdleParkingKernel::new(
        EvalLanguage::Js,
        1.0,
        FakeKernel::new(1),
        restart.clone(),
        None,
    ));
    assert!(parked.clone().kernel_tools().is_none());
    parked.close().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn zero_minutes_returns_the_same_kernel() {
    let (restart, _) = restarts(vec![]);
    let kernel: Arc<dyn EvalKernel> = FakeKernel::new(2);
    let wrapped = park_when_idle(EvalLanguage::Js, 0.0, kernel.clone(), restart, None);
    assert!(Arc::ptr_eq(&wrapped, &kernel));
}

#[tokio::test(start_paused = true)]
async fn a_running_cell_keeps_the_kernel_and_the_idle_clock_starts_over_when_it_settles() {
    let first = FakeKernel::new(1);
    let (open, gate) = watch::channel(false);
    *first.run_gate.lock().unwrap() = Some(gate);
    let (restart, _) = restarts(vec![]);
    let parked = Arc::new(IdleParkingKernel::new(
        EvalLanguage::Js,
        1.0,
        first.clone(),
        restart,
        None,
    ));

    let running = tokio::spawn({
        let parked = parked.clone();
        async move { parked.run(cell("long")).await }
    });
    tokio::time::sleep(5 * MINUTE).await;
    assert!(!first.is_closed());

    open.send_replace(true);
    running.await.unwrap().unwrap();
    let settled_at = tokio::time::Instant::now();
    first.wait_closed().await;
    assert_eq!(
        settled_at.elapsed(),
        MINUTE,
        "the idle clock restarted when the cell settled"
    );
    parked.close().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_cell_submitted_while_the_parked_kernel_closes_runs_on_the_fresh_kernel() {
    let (first, second) = (FakeKernel::new(1), FakeKernel::new(2));
    let (release, gate) = watch::channel(false);
    *first.close_gate.lock().unwrap() = Some(gate);
    let (restart, _) = restarts(vec![Ok(second.clone())]);
    let parked = Arc::new(IdleParkingKernel::new(
        EvalLanguage::Js,
        1.0,
        first.clone(),
        restart,
        None,
    ));

    tokio::time::sleep(MINUTE).await;
    let submitted = tokio::spawn({
        let parked = parked.clone();
        async move { parked.run(cell("during-close")).await }
    });
    tokio::task::yield_now().await;
    assert!(second.ran().is_empty());
    assert_eq!(parked.queue_snapshot().1, ["during-close"]);

    release.send_replace(true);
    let result = submitted.await.unwrap().unwrap();
    assert!(first.ran().is_empty());
    assert_eq!(second.ran(), ["during-close"]);
    assert_eq!(result["kernelState"], "restarted");
    parked.close().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn kernel_tools_stay_reachable_and_a_tool_call_keeps_the_kernel_from_being_parked() {
    let first = FakeKernel::new(1);
    let (release, gate) = watch::channel(false);
    *first.tool_gate.lock().unwrap() = Some(gate);
    let (restart, _) = restarts(vec![]);
    let parked = Arc::new(IdleParkingKernel::new(
        EvalLanguage::Js,
        1.0,
        first.clone(),
        restart,
        None,
    ));
    let tools = parked
        .clone()
        .kernel_tools()
        .expect("wrapper exposes the kernel's tools");

    tools.describe(&["add".into()]).await.unwrap();
    assert_eq!(*first.described.lock().unwrap(), [vec!["add".to_owned()]]);
    let calling = tokio::spawn({
        let tools = tools.clone();
        async move {
            tools
                .invoke(
                    maho_ext_api::KernelToolInvokeRequest {
                        name: "add".into(),
                        kernel_generation: 1,
                        definition_revision: 1,
                        args: json!({}),
                        call_id: "idle-parking-probe".into(),
                    },
                    Default::default(),
                )
                .await
        }
    });
    tokio::time::sleep(5 * MINUTE).await;
    assert!(!first.is_closed());

    release.send_replace(true);
    assert_eq!(calling.await.unwrap().unwrap(), json!("done"));
    first.wait_closed().await;
    let error = tools.describe(&["add".into()]).await.unwrap_err();
    assert_eq!(error.code.as_deref(), Some("tools_unavailable"));
    parked.close().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_dropped_run_waiting_for_the_fresh_kernel_leaves_no_queued_cell() {
    let first = FakeKernel::new(1);
    let (release, gate) = watch::channel(false);
    *first.close_gate.lock().unwrap() = Some(gate);
    let (restart, calls) = restarts(vec![]);
    let parked = Arc::new(IdleParkingKernel::new(
        EvalLanguage::Js,
        1.0,
        first.clone(),
        restart,
        None,
    ));

    tokio::time::sleep(MINUTE).await;
    let submitted = tokio::spawn({
        let parked = parked.clone();
        async move { parked.run(cell("abandoned")).await }
    });
    tokio::task::yield_now().await;
    assert_eq!(parked.queue_snapshot().1, ["abandoned"]);

    submitted.abort();
    assert!(submitted.await.unwrap_err().is_cancelled());
    assert!(parked.queue_snapshot().1.is_empty());

    release.send_replace(true);
    first.wait_closed().await;
    parked.close().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
