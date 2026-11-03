use crate::{
    errors::McpError,
    log::{McpLogger, redact_mcp_log_text},
};
use futures::{FutureExt, StreamExt};
use serde_json::{Value, json};
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::task::JoinHandle;
pub type ErrorNotify = Arc<
    dyn Fn(String, McpError) -> Pin<Box<dyn Future<Output = Result<(), McpError>> + Send>>
        + Send
        + Sync,
>;
pub type ErrorLogger = Arc<dyn Fn(&str, &Value) -> Result<(), String> + Send + Sync>;
#[derive(Clone)]
pub struct McpAsyncErrorSink {
    pub logger: ErrorLogger,
    pub notify: Option<ErrorNotify>,
}
impl McpAsyncErrorSink {
    pub fn from_logger(logger: Arc<Mutex<McpLogger>>) -> Self {
        Self {
            logger: Arc::new(move |scope, data| {
                logger
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .log("error", scope, Some(data), None)
                    .map_err(|e| e.to_string())
            }),
            notify: None,
        }
    }
}
pub async fn wrap_async(
    scope: &str,
    callback: impl Future<Output = Result<(), McpError>>,
    sink: &McpAsyncErrorSink,
) {
    let result = std::panic::AssertUnwindSafe(callback).catch_unwind().await;
    match result {
        Ok(Ok(())) => (),
        Ok(Err(error)) => report_mcp_async_error(scope, error, sink).await,
        Err(payload) => report_mcp_async_error(scope, panic_error(payload), sink).await,
    }
}
fn panic_error(payload: Box<dyn std::any::Any + Send>) -> McpError {
    let message = payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| {
            payload
                .downcast_ref::<&str>()
                .map(|message| (*message).to_owned())
        })
        .unwrap_or_else(|| "Non-string panic".into());
    McpError::new(crate::errors::McpErrorKind::Protocol, message)
}
pub async fn report_mcp_async_error(scope: &str, error: McpError, sink: &McpAsyncErrorSink) {
    log_error(scope, &error, sink);
    if let Some(notify) = &sink.notify {
        let result = std::panic::AssertUnwindSafe(async {
            notify(format!("MCP {scope} failed: {}", error.message), error).await
        })
        .catch_unwind()
        .await;
        match result {
            Ok(Ok(())) => (),
            Ok(Err(error)) => log_error(&format!("{scope}.notify"), &error, sink),
            Err(payload) => log_error(&format!("{scope}.notify"), &panic_error(payload), sink),
        }
    }
}
fn log_error(scope: &str, error: &McpError, sink: &McpAsyncErrorSink) {
    let data = json!({"name":"Error","message":error.message});
    if !matches!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (sink.logger)(scope, &data))),
        Ok(Ok(()))
    ) {
        match redact_mcp_log_text(scope) {
            Ok(scope) => maho_ext_api::diagnostic(&format!(
                "MCP {scope} logger failed; suppressed async error details"
            )),
            Err(_) => maho_ext_api::diagnostic("MCP logger failed; suppressed async error details"),
        }
    }
}
pub fn safe_timer<F, Fut>(
    scope: String,
    delay: Duration,
    callback: F,
    sink: McpAsyncErrorSink,
) -> JoinHandle<()>
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = Result<(), McpError>> + Send + 'static,
{
    let deadline = tokio::time::Instant::now() + delay;
    tokio::spawn(async move {
        tokio::time::sleep_until(deadline).await;
        wrap_async(&scope, callback(), &sink).await;
    })
}
pub fn safe_interval<F, Fut>(
    scope: String,
    delay: Duration,
    mut callback: F,
    sink: McpAsyncErrorSink,
) -> JoinHandle<()>
where
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = Result<(), McpError>> + Send + 'static,
{
    let start = tokio::time::Instant::now() + delay.max(Duration::from_millis(1));
    tokio::spawn(async move {
        let mut interval = tokio::time::interval_at(start, delay.max(Duration::from_millis(1)));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            wrap_async(&scope, callback(), &sink).await;
        }
    })
}
pub async fn safe_delay(delay: Duration) {
    tokio::time::sleep(delay).await;
}

pub fn safe_on<T, F, Fut>(
    mut events: tokio::sync::broadcast::Receiver<T>,
    scope: String,
    mut listener: F,
    sink: McpAsyncErrorSink,
) -> JoinHandle<()>
where
    T: Clone + Send + 'static,
    F: FnMut(T) -> Fut + Send + 'static,
    Fut: Future<Output = Result<(), McpError>> + Send + 'static,
{
    tokio::spawn(async move {
        let mut callbacks = futures::stream::FuturesUnordered::new();
        let mut closed = false;
        loop {
            tokio::select! {
            event=events.recv(),if !closed=>match event {
                Ok(event)=>{
                    let callback=std::panic::catch_unwind(std::panic::AssertUnwindSafe(||listener(event)));
                    let scope=scope.clone();let sink=sink.clone();
                    callbacks.push(async move {
                        match callback {
                            Ok(callback)=>wrap_async(&scope,callback,&sink).await,
                            Err(payload)=>report_mcp_async_error(&scope,panic_error(payload),&sink).await,
                        }
                    });
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_))=>(),
                Err(tokio::sync::broadcast::error::RecvError::Closed)=>closed=true,
            },
            _=callbacks.next(),if !callbacks.is_empty()=>(),
            else=>return,
            }
        }
    })
}

/// Upstream `McpClock`: the modern operation uses one injectable monotonic clock and scheduler.
pub trait McpClock: Send + Sync {
    /// Monotonic milliseconds.
    fn now(&self) -> f64;
    /// Resolves after `ms` monotonic milliseconds (upstream `schedule`; cancel = drop).
    fn sleep(&self, ms: f64) -> Pin<Box<dyn Future<Output = ()> + Send>>;
}
static ELAPSED_BASE: std::sync::LazyLock<tokio::time::Instant> =
    std::sync::LazyLock::new(tokio::time::Instant::now);
/// Upstream `mcpElapsedClock` (`performance.now()` + `setTimeout`), on tokio time so paused-clock tests drive it.
#[derive(Clone, Copy, Debug, Default)]
pub struct McpElapsedClock;
impl McpClock for McpElapsedClock {
    fn now(&self) -> f64 {
        tokio::time::Instant::now()
            .saturating_duration_since(*ELAPSED_BASE)
            .as_secs_f64()
            * 1000.0
    }
    fn sleep(&self, ms: f64) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        let ms = if ms.is_finite() {
            ms.max(0.0)
        } else {
            f64::from(u32::MAX)
        };
        Box::pin(tokio::time::sleep(Duration::from_secs_f64(ms / 1000.0)))
    }
}
pub static MCP_ELAPSED_CLOCK: McpElapsedClock = McpElapsedClock;

/// The awaited work lost to its abort signal (upstream rejects with `signal.reason`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("MCP operation aborted")]
pub struct McpInterrupted;

pub async fn mcp_abortable_delay(
    clock: &dyn McpClock,
    ms: f64,
    signal: &maho_ext_api::AbortSignal,
) -> Result<(), McpInterrupted> {
    tokio::task::yield_now().await;
    if signal.is_aborted() {
        return Err(McpInterrupted);
    }
    tokio::select! {
        biased;
        () = signal.cancelled() => Err(McpInterrupted),
        () = clock.sleep(ms) => Ok(()),
    }
}

pub async fn mcp_interrupted<T>(
    work: impl Future<Output = T>,
    signal: &maho_ext_api::AbortSignal,
) -> Result<T, McpInterrupted> {
    if signal.is_aborted() {
        return Err(McpInterrupted);
    }
    tokio::select! {
        biased;
        () = signal.cancelled() => Err(McpInterrupted),
        value = work => Ok(value),
    }
}

/// Upstream `AbortSignal.any`: a signal aborted by the first of its sources. The forwarder ends with the handle.
pub struct McpLinkedSignal {
    pub signal: maho_ext_api::AbortSignal,
    task: Option<JoinHandle<()>>,
}
impl McpLinkedSignal {
    pub fn new(sources: Vec<maho_ext_api::AbortSignal>) -> Self {
        let signal = maho_ext_api::AbortSignal::default();
        if sources.iter().any(maho_ext_api::AbortSignal::is_aborted) {
            signal.abort();
            return Self { signal, task: None };
        }
        if sources.is_empty() {
            return Self { signal, task: None };
        }
        let target = signal.clone();
        let task = tokio::spawn(async move {
            let waits = sources
                .iter()
                .map(|source| Box::pin(source.cancelled()))
                .collect::<Vec<_>>();
            futures::future::select_all(waits).await;
            target.abort();
        });
        Self {
            signal,
            task: Some(task),
        }
    }
}
impl McpLinkedSignal {
    /// Like `new`, also following host signals: maho-core's presentation and form contract uses the
    /// `maho_ai` abort signal, while this crate's request plumbing uses the tool-level one.
    pub fn with_host(
        sources: Vec<maho_ext_api::AbortSignal>,
        host: Vec<maho_ai::utils::abort::AbortSignal>,
    ) -> Self {
        if host.iter().any(maho_ai::utils::abort::AbortSignal::aborted) {
            let signal = maho_ext_api::AbortSignal::default();
            signal.abort();
            return Self { signal, task: None };
        }
        if host.is_empty() {
            return Self::new(sources);
        }
        let signal = maho_ext_api::AbortSignal::default();
        if sources.iter().any(maho_ext_api::AbortSignal::is_aborted) {
            signal.abort();
            return Self { signal, task: None };
        }
        let target = signal.clone();
        let task = tokio::spawn(async move {
            let waits = sources
                .iter()
                .map(|source| source.cancelled().boxed())
                .chain(host.iter().map(|source| source.cancelled().boxed()));
            futures::future::select_all(waits).await;
            target.abort();
        });
        Self {
            signal,
            task: Some(task),
        }
    }
}

/// A host (`maho_ai`) signal that follows a tool-level signal, for maho-core's form contract. The follower
/// task ends when the guard drops, so a signal that never aborts leaks nothing.
pub struct McpHostSignal {
    pub signal: maho_ai::utils::abort::AbortSignal,
    task: Option<JoinHandle<()>>,
}
impl McpHostSignal {
    pub fn follow(source: &maho_ext_api::AbortSignal) -> Self {
        let controller = maho_ai::utils::abort::AbortController::new();
        let signal = controller.signal();
        if source.is_aborted() {
            controller.abort(None);
            return Self { signal, task: None };
        }
        let source = source.clone();
        let task = tokio::spawn(async move {
            source.cancelled().await;
            controller.abort(None);
        });
        Self {
            signal,
            task: Some(task),
        }
    }
}
impl Drop for McpHostSignal {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

impl Drop for McpLinkedSignal {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// Upstream `onMcpClientClose`: observe client close without replacing another request's observer.
/// Dropping the observer unsubscribes.
pub struct McpCloseObserver(JoinHandle<()>);
impl Drop for McpCloseObserver {
    fn drop(&mut self) {
        self.0.abort();
    }
}
pub fn on_mcp_client_close(
    client: &crate::transport_sdk::McpClient,
    listener: impl FnOnce() + Send + 'static,
) -> McpCloseObserver {
    let mut closed = client.closed.subscribe();
    McpCloseObserver(tokio::spawn(async move {
        loop {
            if *closed.borrow_and_update() {
                break;
            }
            if closed.changed().await.is_err() {
                break;
            }
        }
        listener();
    }))
}
