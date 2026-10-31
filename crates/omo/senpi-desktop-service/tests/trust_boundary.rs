use senpi_desktop_service::{NotificationHub, parse_audit_event};
use serde_json::json;
use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};

// omo v5.1.52 (7f81338fbe): an unknown action reads as "unknown" and is delivered; only a
// structurally malformed event is refused before dispatch.
#[test]
fn malformed_audit_event_is_refused_before_dispatch() {
    let valid = json!({"action":"click","target":"desktop","delivery":"background","durationMs":0});
    assert!(parse_audit_event(&valid).is_some());
    let mut unknown = valid.clone();
    unknown["action"] = json!(7);
    assert!(parse_audit_event(&unknown).is_none());
    let hub = NotificationHub::new();
    let delivered = Arc::new(AtomicUsize::new(0));
    let errors = Arc::new(AtomicUsize::new(0));
    let delivered_sink = delivered.clone();
    let error_sink = errors.clone();
    let _audit = hub.on_audit(move |_| { delivered_sink.fetch_add(1, Ordering::SeqCst); });
    let _errors = hub.on_error(move |_| { error_sink.fetch_add(1, Ordering::SeqCst); });
    hub.dispatch("audit", &unknown);
    assert_eq!(delivered.load(Ordering::SeqCst), 0);
    assert_eq!(errors.load(Ordering::SeqCst), 1);
    hub.dispatch("audit", &valid);
    assert_eq!(delivered.load(Ordering::SeqCst), 1);
}
