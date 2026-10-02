use std::sync::{Arc, Mutex, atomic::{AtomicUsize, Ordering}};
use maho_codemode::extension::session_manager_proxy::*;
use maho_ai::utils::abort::AbortController;

struct FakeManager { disposals: AtomicUsize, failure: Option<String> }
impl maho_codemode::tool::eval_tool_options::EvalKernelManager for FakeManager {
    fn get_kernel(&self, language: maho_codemode::tool::types::EvalLanguage) -> maho_codemode::tool::types::EvalKernelFuture<'_, Arc<dyn maho_codemode::tool::types::EvalKernel>> {
        Box::pin(async move { Err(format!("fixture kernel {language:?}")) })
    }
}
impl SessionManagerLifecycle for FakeManager {
    fn dispose(&self) -> SessionDisposeFuture<'_> {
        Box::pin(async { self.disposals.fetch_add(1, Ordering::SeqCst); self.failure.clone().map_or(Ok(()), Err) })
    }
}
fn manager(failure: Option<&str>) -> Arc<FakeManager> {
    Arc::new(FakeManager { disposals:AtomicUsize::new(0), failure:failure.map(str::to_owned) })
}
fn proxy() -> (Arc<SessionManagerProxy>, Arc<Mutex<Vec<String>>>) {
    let failures = Arc::new(Mutex::new(Vec::new()));
    let reported = failures.clone();
    (Arc::new(SessionManagerProxy::new(Arc::new(move |failure| reported.lock().expect("failure list").push(failure.into())))), failures)
}

#[tokio::test]
async fn kernel_delegation_is_gated_by_live_session_generation() {
    use maho_codemode::tool::{eval_tool_options::EvalKernelManager, types::EvalLanguage};
    let (proxy, _) = proxy();
    assert_eq!(proxy.get_kernel(EvalLanguage::Py).await.err().unwrap(), "codemode session has not started");
    assert!(proxy.replace(proxy.begin_replacement(), manager(None)).await);
    assert_eq!(proxy.get_kernel(EvalLanguage::Rb).await.err().unwrap(), "fixture kernel Rb");
    proxy.begin_replacement();
    assert_eq!(proxy.get_kernel(EvalLanguage::Py).await.err().unwrap(), "codemode session manager is disposed");
    proxy.dispose().await;
}

#[tokio::test]
async fn outgoing_teardown_failure_does_not_prevent_installation() {
    let (proxy, failures) = proxy();
    assert_eq!(proxy.assert_eval_execution_allowed(), Err(SessionProxyError::NotStarted));
    let outgoing = manager(Some("close failed"));
    assert!(proxy.replace(proxy.begin_replacement(), outgoing.clone()).await);
    let next = manager(None);
    assert!(proxy.replace(proxy.begin_replacement(), next.clone()).await);
    assert_eq!(outgoing.disposals.load(Ordering::SeqCst), 1);
    assert_eq!(*failures.lock().unwrap(), ["close failed"]);
    assert!(Arc::ptr_eq(&proxy.current().unwrap(), &(next as Arc<dyn SessionManagerLifecycle>)));
    proxy.dispose().await;
}

#[tokio::test]
async fn dispose_reports_failure_and_remains_disposed() {
    let (proxy, failures) = proxy();
    let active = manager(Some("close failed"));
    assert!(proxy.replace(proxy.begin_replacement(), active.clone()).await);
    proxy.dispose().await;
    assert_eq!(active.disposals.load(Ordering::SeqCst), 1);
    assert_eq!(*failures.lock().unwrap(), ["close failed"]);
    assert_eq!(proxy.assert_eval_execution_allowed(), Err(SessionProxyError::Disposed));
}

#[tokio::test]
async fn superseded_replacement_is_retired_and_failure_reported() {
    let (proxy, failures) = proxy();
    let generation = proxy.begin_replacement();
    proxy.begin_replacement();
    let stale = manager(Some("stale close failed"));
    assert!(!proxy.replace(generation, stale.clone()).await);
    assert_eq!(stale.disposals.load(Ordering::SeqCst), 1);
    assert_eq!(*failures.lock().unwrap(), ["stale close failed"]);
}

#[tokio::test]
async fn replacement_aborts_and_waits_for_tracked_execution() {
    let (proxy, _) = proxy();
    let active = manager(None);
    assert!(proxy.replace(proxy.begin_replacement(), active.clone()).await);
    let controller = AbortController::new();
    let signal = controller.signal();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let execution_proxy = proxy.clone();
    let execution = tokio::spawn(async move {
        execution_proxy.track_eval_execution(async move {
            started_tx.send(()).unwrap();
            signal.cancelled().await;
            assert_eq!(signal.reason().unwrap().name, "CodemodeSessionDisposedError");
            42
        }, controller).await.unwrap()
    });
    started_rx.await.unwrap();
    let generation = proxy.begin_replacement();
    assert!(proxy.replace(generation, manager(None)).await);
    assert_eq!(execution.await.unwrap(), 42);
    assert_eq!(active.disposals.load(Ordering::SeqCst), 1);
    proxy.dispose().await;
}
