//! Port of `extension/idle-parking-kernel.ts` (senpi 903b8336).
//!
//! Opt-in (`memory.idleParkMinutes` > 0): once no cell has been running or queued on the session's
//! kernel for the configured time, closes it to give its memory back, and starts a fresh one when the
//! next cell arrives. That cell's result says the kernel was restarted and every global is gone. A cell
//! submitted while the kernel is being parked or restarted waits for the fresh kernel; a restart that
//! fails settles that cell with the reason, and the next cell tries again.

use crate::kernels::js::kernel_tools_types::KERNEL_TOOLS_INVOKE_SCOPE;
use crate::kernels::shared::kernel_death::{restart_notice, unstarted_result};
use crate::tool::types::{
    EvalKernel, EvalKernelFuture, EvalKernelRunInput, EvalLanguage, KernelInterruptHandle,
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use tokio::sync::{oneshot, watch};

pub type RestartParkedKernel =
    Arc<dyn Fn() -> EvalKernelFuture<'static, Arc<dyn EvalKernel>> + Send + Sync>;
pub type OnParked = Arc<dyn Fn() + Send + Sync>;

const CLOSED: &str = "Kernel closed";

type Restarted = Option<Result<Arc<dyn EvalKernel>, String>>;

#[derive(Default)]
struct State {
    kernel: Option<Arc<dyn EvalKernel>>,
    in_flight: usize,
    timer: Option<tokio::task::AbortHandle>,
    /// Bumped on every arm and disarm; a timer whose generation is stale must not touch `timer` or park.
    timer_gen: u64,
    /// Present while a parked kernel is closing; flips to true once the close finished.
    parking: Option<watch::Receiver<bool>>,
    park_failure: Option<String>,
    /// Present while a restart runs; carries its outcome to every cell that waited on it.
    starting: Option<watch::Receiver<Restarted>>,
    announce: Option<String>,
    waiting: Vec<(String, oneshot::Sender<String>)>,
    closed: bool,
}

struct Inner {
    language: EvalLanguage,
    idle_minutes: f64,
    restart: RestartParkedKernel,
    on_parked: OnParked,
    /// Present only when the wrapped kernel has kernel tools, so a probe for them sees what it saw before.
    has_kernel_tools: bool,
    state: Mutex<State>,
    me: Weak<Inner>,
}

pub struct IdleParkingKernel {
    inner: Arc<Inner>,
}

impl IdleParkingKernel {
    /// Must be called inside a tokio runtime: the idle timer is a spawned task.
    pub fn new(
        language: EvalLanguage,
        idle_minutes: f64,
        first: Arc<dyn EvalKernel>,
        restart: RestartParkedKernel,
        on_parked: Option<OnParked>,
    ) -> Self {
        let has_kernel_tools = first.clone().kernel_tools().is_some();
        let inner = Arc::new_cyclic(|me| Inner {
            language,
            idle_minutes,
            restart,
            on_parked: on_parked.unwrap_or_else(|| Arc::new(|| {})),
            has_kernel_tools,
            state: Mutex::new(State {
                kernel: Some(first),
                ..Default::default()
            }),
            me: me.clone(),
        });
        inner.arm(&mut inner.lock());
        Self { inner }
    }
}

/// `idleMinutes` > 0 wraps the kernel; otherwise the same kernel comes back and no timer is armed.
pub fn park_when_idle(
    language: EvalLanguage,
    idle_minutes: f64,
    kernel: Arc<dyn EvalKernel>,
    restart: RestartParkedKernel,
    on_parked: Option<OnParked>,
) -> Arc<dyn EvalKernel> {
    if idle_minutes > 0.0 {
        Arc::new(IdleParkingKernel::new(
            language,
            idle_minutes,
            kernel,
            restart,
            on_parked,
        ))
    } else {
        kernel
    }
}

fn language_name(language: EvalLanguage) -> &'static str {
    match language {
        EvalLanguage::Js => "js",
    }
}

/// Holds one unit of work on the kernel; dropping it re-arms the idle clock.
struct InFlight(Arc<Inner>);

impl Drop for InFlight {
    fn drop(&mut self) {
        let mut state = self.0.lock();
        state.in_flight -= 1;
        self.0.arm(&mut state);
    }
}

impl Inner {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().expect("idle parking kernel state")
    }

    fn arm(&self, state: &mut State) {
        if state.closed || state.in_flight > 0 || state.kernel.is_none() || state.timer.is_some() {
            return;
        }
        state.timer_gen += 1;
        let generation = state.timer_gen;
        let me = self.me.clone();
        let idle = Duration::from_secs_f64(self.idle_minutes * 60.0);
        let task = tokio::spawn(async move {
            tokio::time::sleep(idle).await;
            if let Some(inner) = me.upgrade() {
                inner.on_timer(generation);
            }
        });
        state.timer = Some(task.abort_handle());
    }

    /// `abort()` cannot stop a timer task already past its sleep, so a disarmed timer can still get
    /// here; its generation tells it apart from the current timer, which it must neither clear nor
    /// pre-empt by parking early.
    fn on_timer(&self, generation: u64) {
        let mut state = self.lock();
        if state.timer_gen != generation {
            return;
        }
        state.timer = None;
        self.park(state);
    }

    fn disarm(state: &mut State) {
        state.timer_gen += 1;
        if let Some(timer) = state.timer.take() {
            timer.abort();
        }
    }

    fn begin(self: &Arc<Self>, state: &mut State) -> InFlight {
        Self::disarm(state);
        state.in_flight += 1;
        InFlight(self.clone())
    }

    fn park(&self, mut state: std::sync::MutexGuard<'_, State>) {
        let Some(kernel) = state.kernel.clone() else {
            return;
        };
        if state.closed || state.in_flight > 0 {
            return;
        }
        let (active, queued) = kernel.queue_snapshot();
        // Tools a cell defined stay callable by child tasks between cells, so their kernel is never parked.
        let has_tools = !kernel.list_kernel_tool_names().is_empty();
        if active.is_some() || !queued.is_empty() || has_tools {
            self.arm(&mut state);
            return;
        }
        state.kernel = None;
        let (done, parking) = watch::channel(false);
        state.parking = Some(parking);
        drop(state);
        (self.on_parked)();
        let me = self.me.clone();
        let language = language_name(self.language);
        tokio::spawn(async move {
            let outcome = kernel.close().await;
            if let Some(inner) = me.upgrade() {
                let mut state = inner.lock();
                if let Err(error) = outcome {
                    state.park_failure = Some(format!(
                        "the idle {language} kernel could not be stopped: {error}; run the cell again to start a fresh kernel"
                    ));
                }
                state.parking = None;
            }
            let _ = done.send(true);
        });
    }

    fn park_reason(&self) -> String {
        let minutes = self.idle_minutes;
        format!(
            "idling for {minutes} minute{} (memory.idleParkMinutes)",
            if minutes == 1.0 { "" } else { "s" }
        )
    }

    fn restart_failure(&self, error: &str) -> String {
        format!(
            "the {} kernel parked after idling did not restart: {error}; run the cell again to retry",
            language_name(self.language)
        )
    }

    async fn current(self: &Arc<Self>) -> Result<Arc<dyn EvalKernel>, String> {
        let parking = self.lock().parking.clone();
        if let Some(mut parking) = parking {
            let _ = parking.wait_for(|done| *done).await;
        }
        let mut starting = {
            let mut state = self.lock();
            if let Some(failure) = state.park_failure.take() {
                return Err(failure);
            }
            if let Some(kernel) = &state.kernel {
                return Ok(kernel.clone());
            }
            if state.closed {
                return Err(CLOSED.into());
            }
            if let Some(starting) = state.starting.clone() {
                starting
            } else {
                let (sender, receiver) = watch::channel(None);
                state.starting = Some(receiver.clone());
                let inner = self.clone();
                tokio::spawn(async move {
                    let outcome = (inner.restart)().await;
                    {
                        let mut state = inner.lock();
                        if let Ok(kernel) = &outcome {
                            state.kernel = Some(kernel.clone());
                            state.announce =
                                Some(restart_notice(inner.language, &inner.park_reason()));
                        }
                        state.starting = None;
                    }
                    let _ = sender.send(Some(outcome));
                });
                receiver
            }
        };
        let outcome = starting
            .wait_for(Option::is_some)
            .await
            .map_err(|_| "kernel restart was dropped".to_owned())?
            .clone();
        outcome.expect("settled restart outcome")
    }

    fn announced(&self, mut result: Value) -> Value {
        let Some(notice) = self.lock().announce.take() else {
            return result;
        };
        let notice = match result["notice"].as_str() {
            Some(existing) => format!("{notice}\n{existing}"),
            None => notice,
        };
        result["notice"] = json!(notice);
        result["kernelState"] = json!("restarted");
        result
    }

    fn cancel_waiting(&self, cell_id: &str, reason: &str) -> bool {
        let mut state = self.lock();
        let Some(index) = state.waiting.iter().position(|(id, _)| id == cell_id) else {
            return false;
        };
        let (_, cancel) = state.waiting.remove(index);
        let _ = cancel.send(reason.to_owned());
        true
    }

    fn cancel_all_waiting(&self, reason: &str) {
        let waiting = std::mem::take(&mut self.lock().waiting);
        for (_, cancel) in waiting {
            let _ = cancel.send(reason.to_owned());
        }
    }

    async fn run_on_current(self: &Arc<Self>, input: EvalKernelRunInput) -> Result<Value, String> {
        let (direct, cancelled) = {
            let mut state = self.lock();
            match (&state.kernel, &state.parking) {
                (Some(kernel), None) => (Some(kernel.clone()), None),
                _ => {
                    let (cancel, cancelled) = oneshot::channel();
                    state.waiting.push((input.cell_id.clone(), cancel));
                    (None, Some(cancelled))
                }
            }
        };
        if let Some(kernel) = direct {
            let result = kernel.run(input).await?;
            return Ok(self.announced(result));
        }
        let cell_id = input.cell_id.clone();
        let mut cancelled = cancelled.expect("waiting cell");
        // Dropping this future while it waits must not leave a phantom queued cell behind.
        let waiting = WaitingCell {
            inner: &**self,
            cell_id: &cell_id,
        };
        let current = tokio::select! {
            reason = &mut cancelled => {
                return Ok(unstarted_result(&cell_id, &reason.unwrap_or_else(|_| CLOSED.into())));
            }
            current = self.current() => current,
        };
        drop(waiting);
        if let Ok(reason) = cancelled.try_recv() {
            return Ok(unstarted_result(&cell_id, &reason));
        }
        match current {
            Ok(kernel) => {
                let result = kernel.run(input).await?;
                Ok(self.announced(result))
            }
            Err(error) => Ok(unstarted_result(&cell_id, &self.restart_failure(&error))),
        }
    }

    fn tools_unavailable(&self) -> maho_ext_api::ExtensionFailure {
        let error = crate::kernels::js::kernel_tools_errors::kernel_tool_error(
            crate::kernels::js::kernel_tools_errors::KernelToolErrorCode::ToolsUnavailable,
            format!(
                "the {} kernel was parked after idling, so the tools it defined are gone; run a cell to start a fresh kernel",
                language_name(self.language)
            ),
            None,
        );
        let mut failure = maho_ext_api::ExtensionFailure::new(error.message);
        failure.class = Some(error.name);
        failure.code = Some("tools_unavailable".into());
        failure
    }

    /// A tool call is work on the kernel: it keeps the kernel from being parked under it.
    fn with_tools(
        self: &Arc<Self>,
    ) -> Result<
        (Arc<dyn maho_ext_api::ExtensionKernelTools>, InFlight),
        maho_ext_api::ExtensionFailure,
    > {
        let mut state = self.lock();
        let kernel = if state.parking.is_none() {
            state.kernel.clone()
        } else {
            None
        };
        let tools = kernel.and_then(|kernel| kernel.kernel_tools());
        match tools {
            Some(tools) if !state.closed => Ok((tools, self.begin(&mut state))),
            _ => Err(self.tools_unavailable()),
        }
    }
}

/// Removes a cell's entry from `waiting` however its wait ends, including the future being dropped.
struct WaitingCell<'a> {
    inner: &'a Inner,
    cell_id: &'a str,
}

impl Drop for WaitingCell<'_> {
    fn drop(&mut self) {
        self.inner
            .lock()
            .waiting
            .retain(|(id, _)| id != self.cell_id);
    }
}

struct ParkedKernelTools(Arc<Inner>);

impl maho_ext_api::ExtensionKernelTools for ParkedKernelTools {
    fn invoke_scope(&self) -> bool {
        KERNEL_TOOLS_INVOKE_SCOPE
    }
    fn describe<'a>(&'a self, names: &'a [String]) -> maho_ext_api::ExtensionFuture<'a, Value> {
        Box::pin(async move {
            let (tools, _in_flight) = self.0.with_tools()?;
            tools.describe(names).await
        })
    }
    fn invoke(
        &self,
        request: maho_ext_api::KernelToolInvokeRequest,
        options: maho_ext_api::KernelToolInvokeOptions,
    ) -> maho_ext_api::ExtensionFuture<'_, Value> {
        Box::pin(async move {
            let (tools, _in_flight) = self.0.with_tools()?;
            tools.invoke(request, options).await
        })
    }
}

impl EvalKernel for IdleParkingKernel {
    fn kernel_tools(self: Arc<Self>) -> Option<Arc<dyn maho_ext_api::ExtensionKernelTools>> {
        self.inner
            .has_kernel_tools
            .then(|| -> Arc<dyn maho_ext_api::ExtensionKernelTools> {
                Arc::new(ParkedKernelTools(self.inner.clone()))
            })
    }

    fn run(&self, input: EvalKernelRunInput) -> EvalKernelFuture<'_, Value> {
        Box::pin(async move {
            let in_flight = {
                let mut state = self.inner.lock();
                if state.closed {
                    return Err(CLOSED.into());
                }
                self.inner.begin(&mut state)
            };
            let result = self.inner.run_on_current(input).await;
            drop(in_flight);
            result
        })
    }

    fn cancel_queued<'a>(
        &'a self,
        cell_id: &'a str,
        reason: &'a str,
    ) -> EvalKernelFuture<'a, bool> {
        Box::pin(async move {
            if self.inner.cancel_waiting(cell_id, reason) {
                return Ok(true);
            }
            let kernel = self.inner.lock().kernel.clone();
            match kernel {
                Some(kernel) => kernel.cancel_queued(cell_id, reason).await,
                None => Ok(false),
            }
        })
    }

    fn interrupt<'a>(
        &'a self,
        reason: &'a str,
        cell_id: Option<&'a str>,
    ) -> EvalKernelFuture<'a, KernelInterruptHandle> {
        Box::pin(async move {
            let retained = || KernelInterruptHandle {
                state_retained: Box::pin(async { Ok(true) }),
                note: None,
            };
            match cell_id {
                Some(id) if self.inner.cancel_waiting(id, reason) => return Ok(retained()),
                None => self.inner.cancel_all_waiting(reason),
                Some(_) => {}
            }
            let kernel = self.inner.lock().kernel.clone();
            match kernel {
                Some(kernel) => kernel.interrupt(reason, cell_id).await,
                None => Ok(retained()),
            }
        })
    }

    fn queue_snapshot(&self) -> (Option<String>, Vec<String>) {
        let state = self.inner.lock();
        let (active, mut queued) = state
            .kernel
            .as_ref()
            .map_or((None, Vec::new()), |kernel| kernel.queue_snapshot());
        queued.extend(state.waiting.iter().map(|(id, _)| id.clone()));
        (active, queued)
    }

    fn deliver_tool_reply(&self, message: Value) -> Result<(), String> {
        let kernel = self.inner.lock().kernel.clone();
        kernel.map_or(Ok(()), |kernel| kernel.deliver_tool_reply(message))
    }

    fn reset(&self) -> EvalKernelFuture<'_, ()> {
        Box::pin(async move {
            let kernel = {
                let mut state = self.inner.lock();
                state.announce = None;
                state.kernel.clone()
            };
            match kernel {
                Some(kernel) => kernel.reset().await,
                None => Ok(()),
            }
        })
    }

    fn close(&self) -> EvalKernelFuture<'_, ()> {
        Box::pin(async move {
            let (parking, starting) = {
                let mut state = self.inner.lock();
                state.closed = true;
                Inner::disarm(&mut state);
                (state.parking.clone(), state.starting.clone())
            };
            self.inner.cancel_all_waiting(CLOSED);
            if let Some(mut parking) = parking {
                let _ = parking.wait_for(|done| *done).await;
            }
            if let Some(mut starting) = starting {
                let _ = starting.wait_for(Option::is_some).await;
            }
            let kernel = self.inner.lock().kernel.take();
            match kernel {
                Some(kernel) => kernel.close().await,
                None => Ok(()),
            }
        })
    }

    fn list_kernel_tool_names(&self) -> Vec<String> {
        let kernel = self.inner.lock().kernel.clone();
        kernel.map_or_else(Vec::new, |kernel| kernel.list_kernel_tool_names())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake(watch::Sender<bool>);

    impl EvalKernel for Fake {
        fn run(&self, input: EvalKernelRunInput) -> EvalKernelFuture<'_, Value> {
            Box::pin(async move { Ok(json!({"type":"result","cellId":input.cell_id,"ok":true})) })
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
            (None, Vec::new())
        }
        fn deliver_tool_reply(&self, _: Value) -> Result<(), String> {
            Ok(())
        }
        fn reset(&self) -> EvalKernelFuture<'_, ()> {
            Box::pin(async { Ok(()) })
        }
        fn close(&self) -> EvalKernelFuture<'_, ()> {
            Box::pin(async {
                self.0.send_replace(true);
                Ok(())
            })
        }
    }

    /// The race needs a multi-threaded runtime (the timer task is past its sleep when `abort()` runs),
    /// so the stale timer's wake is delivered directly with the generation it captured.
    #[tokio::test(start_paused = true)]
    async fn a_timer_that_wakes_after_a_re_arm_neither_parks_nor_clears_the_new_timer() {
        let (closed, mut watch_closed) = watch::channel(false);
        let restart: RestartParkedKernel =
            Arc::new(|| -> EvalKernelFuture<'static, Arc<dyn EvalKernel>> {
                Box::pin(async { Err("no restart expected".to_owned()) })
            });
        let kernel =
            IdleParkingKernel::new(EvalLanguage::Js, 1.0, Arc::new(Fake(closed)), restart, None);
        let stale = kernel.inner.lock().timer_gen;

        tokio::time::sleep(Duration::from_secs(30)).await;
        let input = EvalKernelRunInput {
            cell_id: "re-arm".into(),
            ..Default::default()
        };
        kernel.run(input).await.unwrap();
        kernel.inner.on_timer(stale);
        {
            let state = kernel.inner.lock();
            assert!(state.kernel.is_some(), "the stale timer parked the kernel");
            assert!(
                state.timer.is_some(),
                "the stale timer cleared the new timer"
            );
        }
        assert!(!*watch_closed.borrow());

        let rearmed_at = tokio::time::Instant::now();
        watch_closed.wait_for(|closed| *closed).await.unwrap();
        assert_eq!(rearmed_at.elapsed(), Duration::from_secs(60));
    }
}
