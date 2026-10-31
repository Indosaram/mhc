// Port of senpi test/mcp/modern-subscriptions.test.ts (#4193): connection-owned `subscriptions/listen`
// streams. The upstream transport-level case drives `ModernSdkBridge` / `probeStdioTransport`; the port's
// bridge is `McpClient`, which forwards every message to the same `ModernSubscriptions` routing, so these
// cases exercise that routing directly with a recording send closure and paused tokio time.
use futures::future::BoxFuture;
use maho_ext_mcp::modern::subscriptions::{ModernSubscriptions, SubscriptionSend};
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::time::Instant;

const KEY: &str = "io.modelcontextprotocol/subscriptionId";

fn capabilities() -> Value {
    json!({
        "tools": {"listChanged": true},
        "prompts": {"listChanged": true},
        "resources": {"subscribe": true, "listChanged": true},
    })
}
fn list(id: u64) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "tools/list"})
}
fn resources_list(id: u64, cursor: Option<&str>) -> Value {
    match cursor {
        Some(cursor) => {
            json!({"jsonrpc": "2.0", "id": id, "method": "resources/list", "params": {"cursor": cursor}})
        }
        None => json!({"jsonrpc": "2.0", "id": id, "method": "resources/list"}),
    }
}
fn resources_page(id: u64, uris: &[&str], next: Option<&str>) -> Value {
    let resources: Vec<Value> = uris.iter().map(|uri| json!({"uri": uri})).collect();
    let mut result = json!({"resources": resources});
    if let Some(next) = next {
        result["nextCursor"] = json!(next);
    }
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}
fn notification(method: &str, id: &Value, notifications: Option<Value>) -> Value {
    let mut params = json!({"_meta": {KEY: id}});
    if let Some(notifications) = notifications {
        params["notifications"] = notifications;
    }
    json!({"jsonrpc": "2.0", "method": method, "params": params})
}
fn resource_updated(id: &Value, uri: Option<&str>) -> Value {
    let mut params = json!({"_meta": {KEY: id}});
    if let Some(uri) = uri {
        params["uri"] = json!(uri);
    }
    json!({"jsonrpc": "2.0", "method": "notifications/resources/updated", "params": params})
}

/// Every sent message with the virtual instant it was handed to the transport.
type Sent = Arc<Mutex<Vec<(Instant, Value)>>>;
type Respond = Box<dyn Fn(&Value) -> BoxFuture<'static, Result<(), String>> + Send + Sync>;

fn recording(respond: Respond) -> (Sent, SubscriptionSend) {
    let sent: Sent = Arc::default();
    let seen = sent.clone();
    let send: SubscriptionSend = Arc::new(move |message: Value| {
        let reply = respond(&message);
        seen.lock().unwrap().push((Instant::now(), message));
        reply
    });
    (sent, send)
}
fn ok() -> Respond {
    Box::new(|_| Box::pin(async { Ok(()) }))
}
fn messages(sent: &Sent) -> Vec<Value> {
    sent.lock()
        .unwrap()
        .iter()
        .map(|(_, message)| message.clone())
        .collect()
}
fn is_method(message: &Value, method: &str) -> bool {
    message.get("method").and_then(Value::as_str) == Some(method)
}
fn listen_id(sent: &Sent) -> Value {
    messages(sent)
        .into_iter()
        .rev()
        .find(|message| is_method(message, "subscriptions/listen"))
        .expect("missing listen request")["id"]
        .clone()
}
fn cancellations(sent: &Sent) -> Vec<Value> {
    messages(sent)
        .into_iter()
        .filter(|message| is_method(message, "notifications/cancelled"))
        .map(|message| message["params"]["requestId"].clone())
        .collect()
}
fn last(sent: &Sent) -> Value {
    messages(sent).last().cloned().expect("nothing sent")
}
/// Lets the spawned stream, cancel, and retry tasks run. Paused time never advances while tasks are ready,
/// so this only drains already-runnable work on the current-thread runtime.
async fn settle() {
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
}
async fn advance(ms: u64) {
    tokio::time::advance(Duration::from_millis(ms)).await;
    settle().await;
}

/// Upstream "%s correlates acknowledgments, honors the accepted subset, and cancels on close". `completes`
/// selects the HTTP shape (the listen send stays pending for the SSE lifetime) or the stdio shape.
async fn correlates_acknowledgments(completes: bool) {
    let respond: Respond = if completes {
        Box::new(|message| {
            if is_method(message, "subscriptions/listen") {
                Box::pin(std::future::pending())
            } else {
                Box::pin(async { Ok(()) })
            }
        })
    } else {
        ok()
    };
    let (sent, send) = recording(respond);
    let subscriptions = ModernSubscriptions::new(send, capabilities(), completes);
    let delivered = std::cell::RefCell::new(Vec::new());
    let incoming = |message: Value| {
        if subscriptions.incoming(&message) {
            delivered.borrow_mut().push(message);
        }
    };
    subscriptions.outgoing(&list(1));
    settle().await;
    let id = listen_id(&sent);
    let other = json!("other");
    incoming(notification("notifications/tools/list_changed", &id, None)); // Before acknowledgment.
    incoming(notification(
        "notifications/subscriptions/acknowledged",
        &other,
        Some(json!({"toolsListChanged": true})),
    ));
    incoming(notification("notifications/tools/list_changed", &id, None));
    assert!(delivered.borrow().is_empty());
    incoming(notification(
        "notifications/subscriptions/acknowledged",
        &id,
        Some(json!({"toolsListChanged": true})),
    ));
    incoming(notification(
        "notifications/tools/list_changed",
        &other,
        None,
    ));
    incoming(notification(
        "notifications/prompts/list_changed",
        &id,
        None,
    )); // Not accepted.
    incoming(notification("notifications/resources/updated", &id, None));
    incoming(notification("notifications/tools/list_changed", &id, None));
    assert_eq!(
        *delivered.borrow(),
        vec![notification("notifications/tools/list_changed", &id, None)]
    );
    subscriptions.dispose();
    settle().await;
    assert_eq!(cancellations(&sent), vec![id]);
}

#[tokio::test(start_paused = true)]
async fn http_correlates_acknowledgments_honors_the_accepted_subset_and_cancels_on_close() {
    correlates_acknowledgments(true).await;
}

#[tokio::test(start_paused = true)]
async fn stdio_correlates_acknowledgments_honors_the_accepted_subset_and_cancels_on_close() {
    correlates_acknowledgments(false).await;
}

#[tokio::test(start_paused = true)]
async fn http_completion_without_a_graceful_response_retries_instead_of_retaining_a_dead_stream() {
    let (sent, send) = recording(ok());
    let subscriptions = ModernSubscriptions::new(send, capabilities(), true);
    let start = Instant::now();
    subscriptions.outgoing(&list(1));
    settle().await;
    advance(1000).await;
    let sent_at: Vec<Duration> = sent
        .lock()
        .unwrap()
        .iter()
        .map(|(at, _)| *at - start)
        .collect();
    assert_eq!(sent_at, vec![Duration::ZERO, Duration::from_millis(1000)]);
    subscriptions.dispose();
    settle().await;
    let before = messages(&sent)
        .into_iter()
        .filter(|message| is_method(message, "subscriptions/listen"))
        .count();
    // No timer survives dispose: no further listen is ever opened.
    advance(120_000).await;
    let after = messages(&sent)
        .into_iter()
        .filter(|message| is_method(message, "subscriptions/listen"))
        .count();
    assert_eq!((before, after), (2, 2));
}

async fn graceful_response_ends_the_stream(with_meta: bool) {
    let (sent, send) = recording(ok());
    let subscriptions = ModernSubscriptions::new(send, capabilities(), false);
    subscriptions.outgoing(&list(1));
    settle().await;
    let id = listen_id(&sent);
    let mut result = json!({"resultType": "complete"});
    if with_meta {
        result["_meta"] = json!({KEY: id});
    }
    assert!(!subscriptions.incoming(&json!({"jsonrpc": "2.0", "id": id, "result": result})));
    assert!(!subscriptions.incoming(&notification("notifications/tools/list_changed", &id, None)));
    // Neither a retry nor a cancellation follows a graceful end.
    advance(120_000).await;
    assert_eq!(messages(&sent).len(), 1);
    subscriptions.dispose();
    settle().await;
    assert_eq!(messages(&sent).len(), 1);
}

#[tokio::test(start_paused = true)]
async fn graceful_response_ends_the_stream_with_subscription_metadata() {
    graceful_response_ends_the_stream(true).await;
}

#[tokio::test(start_paused = true)]
async fn graceful_response_ends_the_stream_without_subscription_metadata() {
    graceful_response_ends_the_stream(false).await;
}

#[tokio::test(start_paused = true)]
async fn abrupt_http_failure_uses_bounded_exponential_backoff_and_dispose_cancels_the_pending_retry()
 {
    let (sent, send) = recording(Box::new(|_| {
        Box::pin(async { Err("Synthetic stream EOF".to_owned()) })
    }));
    let subscriptions = ModernSubscriptions::new(send, capabilities(), false);
    subscriptions.outgoing(&list(1));
    settle().await;
    assert_eq!(messages(&sent).len(), 1);
    advance(999).await;
    assert_eq!(messages(&sent).len(), 1);
    advance(1).await;
    assert_eq!(messages(&sent).len(), 2);
    advance(1999).await;
    assert_eq!(messages(&sent).len(), 2);
    advance(1).await;
    assert_eq!(messages(&sent).len(), 3);
    subscriptions.dispose();
    settle().await;
    advance(120_000).await;
    assert_eq!(messages(&sent).len(), 3);
}

#[tokio::test(start_paused = true)]
async fn paginated_resource_lists_hand_over_delivery_only_after_replacement_acknowledgment() {
    let (sent, send) = recording(ok());
    let subscriptions = ModernSubscriptions::new(send, capabilities(), false);
    subscriptions.outgoing(&list(1));
    settle().await;
    let first = listen_id(&sent);
    subscriptions.incoming(&notification(
        "notifications/subscriptions/acknowledged",
        &first,
        Some(json!({"toolsListChanged": true})),
    ));
    subscriptions.outgoing(&resources_list(2, None));
    subscriptions.incoming(&resources_page(2, &["test://a"], Some("next")));
    settle().await;
    assert_eq!(listen_id(&sent), first);
    subscriptions.outgoing(&resources_list(3, Some("next")));
    subscriptions.incoming(&resources_page(3, &["test://b"], None));
    settle().await;
    let second = listen_id(&sent);
    assert_ne!(second, first);
    assert_eq!(
        last(&sent)["params"]["notifications"]["resourceSubscriptions"],
        json!(["test://a", "test://b"])
    );
    assert!(cancellations(&sent).is_empty());
    assert!(subscriptions.incoming(&notification(
        "notifications/tools/list_changed",
        &first,
        None
    )));
    assert!(!subscriptions.incoming(&notification(
        "notifications/tools/list_changed",
        &second,
        None
    )));
    subscriptions.incoming(&notification(
        "notifications/subscriptions/acknowledged",
        &second,
        Some(json!({"toolsListChanged": true, "resourceSubscriptions": ["test://b", "test://unrequested"]})),
    ));
    settle().await;
    let cancelled = last(&sent);
    assert!(is_method(&cancelled, "notifications/cancelled"));
    assert_eq!(cancelled["params"]["requestId"], first);
    assert!(!subscriptions.incoming(&notification(
        "notifications/tools/list_changed",
        &first,
        None
    )));
    assert!(subscriptions.incoming(&notification(
        "notifications/tools/list_changed",
        &second,
        None
    )));
    assert!(!subscriptions.incoming(&notification(
        "notifications/resources/updated",
        &first,
        None
    )));
    assert!(subscriptions.incoming(&resource_updated(&second, Some("test://b"))));
    subscriptions.dispose();
}

#[tokio::test(start_paused = true)]
async fn resource_updates_reach_refresh_handlers_only_for_an_accepted_uri() {
    let (sent, send) = recording(ok());
    let subscriptions = ModernSubscriptions::new(send, capabilities(), false);
    subscriptions.outgoing(&resources_list(1, None));
    subscriptions.incoming(&resources_page(1, &["test://a", "test://b"], None));
    settle().await;
    let id = listen_id(&sent);
    subscriptions.incoming(&notification(
        "notifications/subscriptions/acknowledged",
        &id,
        Some(json!({"resourceSubscriptions": ["test://a", "test://unrequested"]})),
    ));
    let refreshed: Vec<Option<&str>> = [
        Some("test://a"),
        Some("test://b"),
        Some("test://unrequested"),
        None,
    ]
    .into_iter()
    .filter(|uri| subscriptions.incoming(&resource_updated(&id, *uri)))
    .collect();
    assert_eq!(refreshed, vec![Some("test://a")]);
    subscriptions.dispose();
}

#[tokio::test(start_paused = true)]
async fn acknowledgment_missing_the_required_notifications_object_cancels_and_retries_with_backoff()
{
    let (sent, send) = recording(ok());
    let subscriptions = ModernSubscriptions::new(send, capabilities(), false);
    subscriptions.outgoing(&list(1));
    settle().await;
    let id = listen_id(&sent);
    subscriptions.incoming(&notification(
        "notifications/subscriptions/acknowledged",
        &id,
        None,
    ));
    settle().await;
    let cancelled = last(&sent);
    assert!(is_method(&cancelled, "notifications/cancelled"));
    assert_eq!(cancelled["params"]["requestId"], id);
    assert!(!subscriptions.incoming(&notification("notifications/tools/list_changed", &id, None)));
    advance(999).await;
    assert_eq!(listen_id(&sent), id);
    advance(1).await;
    assert_ne!(listen_id(&sent), id);
    subscriptions.dispose();
}

#[tokio::test(start_paused = true)]
async fn quick_acknowledged_disconnects_grow_backoff_until_an_accepted_notification_proves_health()
{
    let drops: Arc<Mutex<Vec<tokio::sync::oneshot::Sender<()>>>> = Arc::default();
    let streams = drops.clone();
    let (sent, send) = recording(Box::new(move |message| {
        if is_method(message, "subscriptions/listen") {
            let (release, released) = tokio::sync::oneshot::channel();
            streams.lock().unwrap().push(release);
            Box::pin(async move {
                let _ = released.await;
                Ok(())
            })
        } else {
            Box::pin(async { Ok(()) })
        }
    }));
    let subscriptions = ModernSubscriptions::new(send, capabilities(), true);
    let drop_latest = || {
        if let Some(release) = drops.lock().unwrap().pop() {
            let _ = release.send(());
        }
    };
    subscriptions.outgoing(&list(1));
    settle().await;
    for delay in [1000, 2000, 4000] {
        let id = listen_id(&sent);
        subscriptions.incoming(&notification(
            "notifications/subscriptions/acknowledged",
            &id,
            Some(json!({"toolsListChanged": true})),
        ));
        drop_latest();
        settle().await;
        advance(delay - 1).await;
        assert_eq!(listen_id(&sent), id, "reopened before {delay}ms");
        advance(1).await;
        assert_ne!(listen_id(&sent), id, "not reopened after {delay}ms");
    }
    let healthy = listen_id(&sent);
    subscriptions.incoming(&notification(
        "notifications/subscriptions/acknowledged",
        &healthy,
        Some(json!({"toolsListChanged": true})),
    ));
    assert!(subscriptions.incoming(&notification(
        "notifications/tools/list_changed",
        &healthy,
        None
    )));
    drop_latest();
    settle().await;
    advance(999).await;
    assert_eq!(listen_id(&sent), healthy);
    advance(1).await;
    assert_ne!(listen_id(&sent), healthy);
    subscriptions.dispose();
}

async fn concurrent_resource_sweeps(latest_first: bool) {
    let (sent, send) = recording(ok());
    let subscriptions = ModernSubscriptions::new(send, capabilities(), false);
    subscriptions.outgoing(&list(1));
    settle().await;
    let initial = listen_id(&sent);
    subscriptions.outgoing(&resources_list(2, None));
    subscriptions.outgoing(&resources_list(3, None));
    subscriptions.incoming(&resources_page(3, &["test://new-a"], Some("new")));
    subscriptions.incoming(&resources_page(2, &["test://old-a"], Some("old")));
    subscriptions.outgoing(&resources_list(4, Some("old")));
    subscriptions.outgoing(&resources_list(5, Some("new")));
    let old = resources_page(4, &["test://old-b"], None);
    let latest = resources_page(5, &["test://new-b"], None);
    if !latest_first {
        subscriptions.incoming(&old);
        settle().await;
        assert_eq!(listen_id(&sent), initial);
    }
    subscriptions.incoming(&latest);
    settle().await;
    let published = listen_id(&sent);
    assert_eq!(
        last(&sent)["params"]["notifications"]["resourceSubscriptions"],
        json!(["test://new-a", "test://new-b"])
    );
    if latest_first {
        subscriptions.incoming(&old);
        settle().await;
        assert_eq!(listen_id(&sent), published);
    }
    subscriptions.dispose();
}

#[tokio::test(start_paused = true)]
async fn concurrent_resource_sweeps_publish_only_the_latest_complete_set_latest_finishes_first() {
    concurrent_resource_sweeps(true).await;
}

#[tokio::test(start_paused = true)]
async fn concurrent_resource_sweeps_publish_only_the_latest_complete_set_latest_finishes_last() {
    concurrent_resource_sweeps(false).await;
}
