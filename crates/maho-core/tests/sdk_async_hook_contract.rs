use std::sync::{Arc, Mutex};
use maho_ai::types::{ProviderRequestOptions, AsyncOnPayload};

fn model() -> maho_ai::types::Model {
    serde_json::from_value(serde_json::json!({
        "id":"fixture","name":"fixture","api":"faux","provider":"faux","baseUrl":"",
        "reasoning":false,"input":[],"contextWindow":128000,"maxTokens":4096,
        "cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0}
    })).expect("model")
}

#[tokio::test]
async fn payload_helpers_preserve_sync_replacement_and_await_async_result() {
    let options = ProviderRequestOptions {
        on_payload: Some(Arc::new(|_, _, _| Some(serde_json::json!({"sync":true})))),
        async_on_payload: Some(Arc::new(|payload, _, _| Box::pin(async move {
            assert_eq!(payload, serde_json::json!({"sync":true}));
            Ok(None)
        }))), ..Default::default()
    };
    assert_eq!(options.apply_payload_hook(&serde_json::json!({"original":true}), &model(), None).await
        .expect("payload hook"), Some(serde_json::json!({"sync":true})));
}

#[tokio::test]
async fn preaborted_requests_do_not_construct_sync_or_async_hook_callbacks() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let calls = Arc::new(AtomicUsize::new(0));
    let controller = maho_ai::utils::abort::AbortController::new();
    controller.abort(None);
    let payload_sync = calls.clone();
    let payload_async = calls.clone();
    let response_sync = calls.clone();
    let response_async = calls.clone();
    let options = ProviderRequestOptions {
        signal: Some(controller.signal()),
        on_payload: Some(Arc::new(move |_, _, _| { payload_sync.fetch_add(1, Ordering::SeqCst); None })),
        async_on_payload: Some(Arc::new(move |_, _, _| { payload_async.fetch_add(1, Ordering::SeqCst); Box::pin(async { Ok(None) }) })),
        on_response: Some(Arc::new(move |_, _| { response_sync.fetch_add(1, Ordering::SeqCst); })),
        async_on_response: Some(Arc::new(move |_, _| { response_async.fetch_add(1, Ordering::SeqCst); Box::pin(async { Ok(()) }) })),
        ..Default::default()
    };
    let model = model();
    let error = maho_ai::utils::abort::AbortReason::dom_default().message;
    assert_eq!(options.apply_payload_hook(&serde_json::json!({}), &model, None).await, Err(error.clone()));
    assert_eq!(options.apply_response_hook(&maho_ai::types::ProviderResponse {
        status: 200, headers: Default::default(),
    }, &model).await, Err(error));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn abort_in_sync_hooks_prevents_async_callback_construction() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    for response in [false, true] {
        let controller = maho_ai::utils::abort::AbortController::new();
        let payload_abort = controller.clone();
        let response_abort = controller.clone();
        let calls = Arc::new(AtomicUsize::new(0));
        let payload_calls = calls.clone();
        let response_calls = calls.clone();
        let options = ProviderRequestOptions {
            signal: Some(controller.signal()),
            on_payload: Some(Arc::new(move |_, _, _| { payload_abort.abort(None); None })),
            on_response: Some(Arc::new(move |_, _| response_abort.abort(None))),
            async_on_payload: Some(Arc::new(move |_, _, _| {
                payload_calls.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Ok(None) })
            })),
            async_on_response: Some(Arc::new(move |_, _| {
                response_calls.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Ok(()) })
            })), ..Default::default()
        };
        let result = if response {
            options.apply_response_hook(&maho_ai::types::ProviderResponse {
                status: 200, headers: Default::default(),
            }, &model()).await
        } else {
            options.apply_payload_hook(&serde_json::json!({}), &model(), None).await.map(|_| ())
        };
        assert_eq!(result, Err(maho_ai::utils::abort::AbortReason::dom_default().message));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn cancellation_drops_pending_payload_future_before_returning() {
    struct Dropped(Arc<Mutex<bool>>);
    impl Drop for Dropped {
        fn drop(&mut self) { *self.0.lock().expect("drop state") = true; }
    }
    let dropped = Arc::new(Mutex::new(false));
    let (entered, observed) = tokio::sync::oneshot::channel();
    let entered = Arc::new(Mutex::new(Some(entered)));
    let controller = maho_ai::utils::abort::AbortController::new();
    let captured = dropped.clone();
    let hook: AsyncOnPayload = Arc::new(move |_, _, _| {
        let guard = Dropped(captured.clone());
        let entered = entered.clone();
        Box::pin(async move {
            let _guard = guard;
            entered.lock().expect("entered").take().expect("single invocation").send(()).expect("observer");
            std::future::pending().await
        })
    });
    let options = ProviderRequestOptions { signal: Some(controller.signal()), async_on_payload: Some(hook), ..Default::default() };
    let result = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        let model = model();
        let payload = serde_json::json!({});
        let (result, ()) = tokio::join!(options.apply_payload_hook(&payload, &model, None), async {
            observed.await.expect("hook entered");
            controller.abort(None);
        });
        result
    }).await.expect("bounded cancellation");
    assert!(result.is_err());
    assert!(*dropped.lock().expect("drop state"));
}

#[tokio::test]
async fn response_helpers_preserve_order_and_propagate_async_failure() {
    let order = Arc::new(Mutex::new(Vec::new()));
    let sync = order.clone();
    let asynchronous = order.clone();
    let options = ProviderRequestOptions {
        on_response: Some(Arc::new(move |_, _| sync.lock().expect("order").push("sync"))),
        async_on_response: Some(Arc::new(move |_, _| {
            let order = asynchronous.clone();
            Box::pin(async move {
                order.lock().expect("order").push("async");
                Err("response hook failed".to_owned())
            })
        })), ..Default::default()
    };
    assert_eq!(options.apply_response_hook(&maho_ai::types::ProviderResponse {
        status: 200, headers: Default::default(),
    }, &model()).await, Err("response hook failed".to_owned()));
    assert_eq!(*order.lock().expect("order"), ["sync", "async"]);
}

#[tokio::test]
async fn cancellation_drops_pending_response_future_before_returning() {
    struct Dropped(Arc<std::sync::atomic::AtomicBool>);
    impl Drop for Dropped {
        fn drop(&mut self) { self.0.store(true, std::sync::atomic::Ordering::SeqCst); }
    }
    let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (entered, observed) = tokio::sync::oneshot::channel();
    let entered = Arc::new(Mutex::new(Some(entered)));
    let controller = maho_ai::utils::abort::AbortController::new();
    let captured = dropped.clone();
    let options = ProviderRequestOptions {
        signal: Some(controller.signal()),
        async_on_response: Some(Arc::new(move |_, _| {
            let guard = Dropped(captured.clone());
            let entered = entered.clone();
            Box::pin(async move {
                let _guard = guard;
                entered.lock().expect("entered").take().expect("single invocation").send(()).expect("observer");
                std::future::pending().await
            })
        })), ..Default::default()
    };
    let result = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        let response = maho_ai::types::ProviderResponse { status: 200, headers: Default::default() };
        let model = model();
        tokio::join!(options.apply_response_hook(&response, &model), async {
            observed.await.expect("response hook entered");
            controller.abort(None);
        }).0
    }).await.expect("bounded response cancellation");
    assert_eq!(result, Err(maho_ai::utils::abort::AbortReason::dom_default().message));
    assert!(dropped.load(std::sync::atomic::Ordering::SeqCst));
}
