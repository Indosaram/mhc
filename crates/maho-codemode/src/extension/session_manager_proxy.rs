use std::{collections::HashMap, future::Future, pin::Pin, sync::{Arc, Mutex}};
use maho_ai::utils::abort::{AbortController, AbortReason};

pub type SessionDisposeFuture<'a> = Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;
pub trait SessionManagerLifecycle: crate::tool::eval_tool_options::EvalKernelManager {
    fn dispose(&self) -> SessionDisposeFuture<'_>;
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SessionProxyError {
    #[error("codemode session has not started")]
    NotStarted,
    #[error("codemode session manager is disposed")]
    Disposed,
}

struct TrackedExecution {
    controller: Arc<AbortController>,
    settled: tokio::sync::watch::Receiver<bool>,
}

#[derive(Default)]
struct ProxyState {
    current: Option<Arc<dyn SessionManagerLifecycle>>,
    generation: u64,
    started: bool,
    accepting_executions: bool,
    next_execution: u64,
    executions: HashMap<u64, TrackedExecution>,
}

pub struct SessionManagerProxy {
    state: Arc<Mutex<ProxyState>>,
    on_teardown_failure: Arc<dyn Fn(&str) + Send + Sync>,
}

impl Default for SessionManagerProxy {
    fn default() -> Self { Self::new(Arc::new(|error| eprintln!("[senpi-codemode] session teardown failed: {error}"))) }
}

struct ExecutionGuard {
    state: Arc<Mutex<ProxyState>>,
    id: u64,
    settled: tokio::sync::watch::Sender<bool>,
}

impl Drop for ExecutionGuard {
    fn drop(&mut self) {
        self.state.lock().expect("session proxy poisoned").executions.remove(&self.id);
        let _ = self.settled.send(true);
    }
}

impl SessionManagerProxy {
    pub fn new(on_teardown_failure: Arc<dyn Fn(&str) + Send + Sync>) -> Self {
        Self { state: Arc::new(Mutex::new(ProxyState::default())), on_teardown_failure }
    }

    pub fn begin_replacement(&self) -> u64 {
        let mut state = self.state.lock().expect("session proxy poisoned");
        state.generation += 1;
        state.accepting_executions = false;
        let generation = state.generation;
        let controllers: Vec<_> = state.executions.values().map(|execution| execution.controller.clone()).collect();
        drop(state);
        for controller in controllers { controller.abort(Some(AbortReason::new("CodemodeSessionDisposedError", "codemode session manager is disposed"))); }
        generation
    }

    pub fn assert_eval_execution_allowed(&self) -> Result<(), SessionProxyError> {
        let state = self.state.lock().expect("session proxy poisoned");
        Self::assert_allowed(&state)
    }

    fn assert_allowed(state: &ProxyState) -> Result<(), SessionProxyError> {
        if state.accepting_executions && state.current.is_some() { Ok(()) }
        else if state.started { Err(SessionProxyError::Disposed) }
        else { Err(SessionProxyError::NotStarted) }
    }

    pub fn current(&self) -> Result<Arc<dyn SessionManagerLifecycle>, SessionProxyError> {
        let state = self.state.lock().expect("session proxy poisoned");
        Self::assert_allowed(&state)?;
        state.current.clone().ok_or(SessionProxyError::NotStarted)
    }

    pub async fn track_eval_execution<T>(&self, execution: impl Future<Output = T>, controller: AbortController) -> Result<T, SessionProxyError> {
        let guard = {
            let mut state = self.state.lock().expect("session proxy poisoned");
            Self::assert_allowed(&state)?;
            let id = state.next_execution;
            state.next_execution += 1;
            let (settled, receiver) = tokio::sync::watch::channel(false);
            state.executions.insert(id, TrackedExecution { controller: Arc::new(controller), settled: receiver });
            ExecutionGuard { state: self.state.clone(), id, settled }
        };
        let result = execution.await;
        drop(guard);
        Ok(result)
    }

    fn is_current(&self, generation: u64) -> bool { self.state.lock().expect("session proxy poisoned").generation == generation }

    async fn dispose_quietly(&self, manager: Option<Arc<dyn SessionManagerLifecycle>>) {
        if let Some(manager) = manager && let Err(error) = manager.dispose().await { (self.on_teardown_failure)(&error); }
    }

    async fn settle_executions(&self) {
        let executions: Vec<_> = self.state.lock().expect("session proxy poisoned").executions.values().map(|execution| execution.settled.clone()).collect();
        for mut execution in executions { let _ = execution.wait_for(|settled| *settled).await; }
    }

    pub async fn replace(&self, generation: u64, next: Arc<dyn SessionManagerLifecycle>) -> bool {
        if !self.is_current(generation) { self.dispose_quietly(Some(next)).await; return false; }
        self.settle_executions().await;
        let outgoing = {
            let mut state = self.state.lock().expect("session proxy poisoned");
            if state.generation == generation { state.current.take() } else { None }
        };
        self.dispose_quietly(outgoing).await;
        let installed = {
            let mut state = self.state.lock().expect("session proxy poisoned");
            if state.generation == generation {
                state.current = Some(next.clone());
                state.started = true;
                state.accepting_executions = true;
                true
            } else { false }
        };
        if !installed { self.dispose_quietly(Some(next)).await; }
        installed
    }

    pub async fn dispose(&self) {
        self.begin_replacement();
        self.settle_executions().await;
        let current = self.state.lock().expect("session proxy poisoned").current.take();
        self.dispose_quietly(current).await;
    }
}

impl crate::tool::eval_tool_options::EvalKernelManager for SessionManagerProxy {
    fn get_kernel(&self, language: crate::tool::types::EvalLanguage) -> crate::tool::types::EvalKernelFuture<'_, Arc<dyn crate::tool::types::EvalKernel>> {
        Box::pin(async move {
            let current = self.current().map_err(|error| error.to_string())?;
            current.get_kernel(language).await
        })
    }
}
