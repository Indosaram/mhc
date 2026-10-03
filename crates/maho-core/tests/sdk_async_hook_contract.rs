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
