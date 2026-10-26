//! Ordered, bounded move hooks. All failures are returned to the move owner.
use futures::future::BoxFuture;
use maho_ai::utils::abort::{AbortController, AbortSignal};
#[derive(Clone, Copy)]
pub struct ExtensionHandlerBudget {
    pub warn_ms: u64,
    pub timeout_ms: u64,
}
#[derive(Debug, Clone)]
pub struct SessionMoveFailure {
    pub extension_path: String,
    pub error: String,
    pub stack: Option<String>,
}
pub struct SessionMoveHandlerCall {
    pub extension_path: String,
    pub call: Box<dyn FnOnce(AbortSignal) -> BoxFuture<'static, Result<(), String>> + Send>,
}
pub async fn run_session_move_handlers(
    event: &str,
    calls: Vec<SessionMoveHandlerCall>,
    budget: ExtensionHandlerBudget,
) -> Vec<SessionMoveFailure> {
    let mut failures = Vec::new();
    for handler in calls {
        let controller = AbortController::new();
        let signal = controller.signal();
        let mut task = tokio::spawn(async move { (handler.call)(signal).await });
        let started = tokio::time::Instant::now();
        let warn =
            tokio::time::sleep_until(started + std::time::Duration::from_millis(budget.warn_ms));
        tokio::pin!(warn);
        let cap =
            tokio::time::sleep_until(started + std::time::Duration::from_millis(budget.timeout_ms));
        tokio::pin!(cap);
        let mut warned = false;
        let error = loop {
            tokio::select! {result=&mut task=>break match result{Ok(Ok(()))=>None,Ok(Err(e))=>Some(e),Err(e)=>Some(e.to_string())},_=&mut warn,if budget.warn_ms>0&&!warned=>{warned=true;eprintln!("Extension {} is still running its {} handler after {}ms.",handler.extension_path,event,budget.warn_ms);},_=&mut cap,if budget.timeout_ms>0=>{controller.abort(None);break Some(format!("handler timed out after {}ms",budget.timeout_ms));}}
        };
        if let Some(error) = error {
            failures.push(SessionMoveFailure {
                extension_path: handler.extension_path,
                error,
                stack: None,
            });
        }
    }
    failures
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn runs_all_handlers_in_order_and_collects_failures() {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let calls = (0..3)
            .map(|i| {
                let seen = seen.clone();
                SessionMoveHandlerCall {
                    extension_path: i.to_string(),
                    call: Box::new(move |_| {
                        Box::pin(async move {
                            seen.lock().expect("seen").push(i);
                            if i == 1 {
                                Err("export failed".into())
                            } else {
                                Ok(())
                            }
                        })
                    }),
                }
            })
            .collect();
        let failures = run_session_move_handlers(
            "session_move_export",
            calls,
            ExtensionHandlerBudget {
                warn_ms: 0,
                timeout_ms: 1000,
            },
        )
        .await;
        assert_eq!(*seen.lock().expect("seen"), vec![0, 1, 2]);
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].extension_path, "1");
    }
}
