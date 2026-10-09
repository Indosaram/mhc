use senpi_desktop_service::{NotificationHub, is_audit_event};
use serde_json::json;
use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};

#[test]
fn unrecognized_audit_method_is_refused_before_dispatch() {
    let valid = json!({"action":"click","target":"desktop","delivery":"background","durationMs":0});
    assert!(is_audit_event(&valid));
    let mut unknown = valid.clone();
    unknown["action"] = json!("unrecognized");
    assert!(!is_audit_event(&unknown));
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
