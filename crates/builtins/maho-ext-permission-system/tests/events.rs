use maho_ext_permission_system::{events::{PermissionEventEmitter,PermissionRepliedEvent},types::{Request,PermissionDecision}};
use std::sync::{Arc,Mutex};
fn request()->Request{Request{id:"req-123".into(),session_id:"sess-456".into(),permission:"bash".into(),patterns:vec!["/home/user/file.txt".into()],always:vec![],metadata:Default::default(),tool:None}}
#[test]
fn asked_bus_payload(){let emitter=PermissionEventEmitter::default();let seen=Arc::new(Mutex::new(None));let captured=seen.clone();let _subscription=emitter.bus.on("permission_asked",Arc::new(move|value|*captured.lock().expect("lock")=Some(value.clone())));emitter.emit_asked(&request()).expect("emit");assert_eq!(seen.lock().expect("lock").as_ref().expect("payload")["id"],"req-123");}
#[test]
fn replied_bus_payload(){let emitter=PermissionEventEmitter::default();let seen=Arc::new(Mutex::new(None));let captured=seen.clone();let _subscription=emitter.bus.on("permission_replied",Arc::new(move|value|*captured.lock().expect("lock")=Some(value.clone())));emitter.emit_replied(PermissionRepliedEvent{request_id:"req-123".into(),session_id:"sess-456".into(),reply:PermissionDecision::Once}).expect("emit");assert_eq!(seen.lock().expect("lock").as_ref().expect("payload"),&serde_json::json!({"requestID":"req-123","sessionID":"sess-456","reply":"once"}));}
#[test]
fn asked_multiple_and_unsubscribe(){let emitter=PermissionEventEmitter::default();let seen=Arc::new(Mutex::new(Vec::new()));let first=seen.clone();let subscription=emitter.on_asked(move|request|first.lock().expect("lock").push(request.id));let second=seen.clone();let _other=emitter.on_asked(move|request|second.lock().expect("lock").push(request.session_id));emitter.emit_asked(&request()).expect("emit");drop(subscription);emitter.emit_asked(&request()).expect("emit");assert_eq!(*seen.lock().expect("lock"),vec!["req-123","sess-456","sess-456"]);}
#[test]
fn replied_multiple_and_unsubscribe(){let emitter=PermissionEventEmitter::default();let seen=Arc::new(Mutex::new(Vec::new()));let first=seen.clone();let subscription=emitter.on_replied(move|event|first.lock().expect("lock").push(event.request_id));let second=seen.clone();let _other=emitter.on_replied(move|event|second.lock().expect("lock").push(event.session_id));for index in 0..2{emitter.emit_replied(PermissionRepliedEvent{request_id:"req".into(),session_id:"sess".into(),reply:PermissionDecision::Always}).expect("emit");if index==0{emitter.clear();}}drop(subscription);assert_eq!(*seen.lock().expect("lock"),vec!["req","sess"]);}
#[test]
fn asked_panic_isolated(){let emitter=PermissionEventEmitter::default();let _panic=emitter.on_asked(|_|panic!("handler"));let count=Arc::new(Mutex::new(0));let captured=count.clone();let _good=emitter.on_asked(move|_|*captured.lock().expect("lock")+=1);emitter.emit_asked(&request()).expect("emit");assert_eq!(*count.lock().expect("lock"),1);}
#[test]
fn replied_panic_isolated(){let emitter=PermissionEventEmitter::default();let _panic=emitter.on_replied(|_|panic!("handler"));let count=Arc::new(Mutex::new(0));let captured=count.clone();let _good=emitter.on_replied(move|_|*captured.lock().expect("lock")+=1);emitter.emit_replied(PermissionRepliedEvent{request_id:"req".into(),session_id:"sess".into(),reply:PermissionDecision::Once}).expect("emit");assert_eq!(*count.lock().expect("lock"),1);}

#[test]
fn clear_removes_both_event_kinds() {
    let emitter = PermissionEventEmitter::default();
    let count = Arc::new(Mutex::new(0));
    let asked_count = count.clone();
    let replied_count = count.clone();
    let _asked = emitter.on_asked(move |_| *asked_count.lock().expect("lock") += 1);
    let _replied = emitter.on_replied(move |_| *replied_count.lock().expect("lock") += 1);

    emitter.clear();
    emitter.emit_asked(&request()).expect("emit");
    emitter.emit_replied(PermissionRepliedEvent {
        request_id: "req".into(), session_id: "sess".into(), reply: PermissionDecision::Always,
    }).expect("emit");

    assert_eq!(*count.lock().expect("lock"), 0);
}

#[test]
fn unsubscribe_replied_preserves_other_listener() {
    let emitter = PermissionEventEmitter::default();
    let removed_count = Arc::new(Mutex::new(0));
    let remaining_count = Arc::new(Mutex::new(0));
    let removed = removed_count.clone();
    let remaining = remaining_count.clone();
    let subscription = emitter.on_replied(move |_| *removed.lock().expect("lock") += 1);
    let _other = emitter.on_replied(move |_| *remaining.lock().expect("lock") += 1);

    drop(subscription);
    emitter.emit_replied(PermissionRepliedEvent {
        request_id: "req".into(), session_id: "sess".into(), reply: PermissionDecision::Reject,
    }).expect("emit");

    assert_eq!(*removed_count.lock().expect("lock"), 0);
    assert_eq!(*remaining_count.lock().expect("lock"), 1);
}

#[test]
fn typed_asked_handler_receives_full_request() {
    let emitter = PermissionEventEmitter::default();
    let seen = Arc::new(Mutex::new(None));
    let captured = seen.clone();
    let _subscription = emitter.on_asked(move |event| *captured.lock().expect("lock") = Some(event));

    emitter.emit_asked(&request()).expect("emit");

    assert_eq!(serde_json::to_value(seen.lock().expect("lock").as_ref().expect("event")).expect("json"), serde_json::to_value(request()).expect("json"));
}

#[test]
fn typed_replied_handler_receives_correlation_and_decision() {
    let emitter = PermissionEventEmitter::default();
    let seen = Arc::new(Mutex::new(None));
    let captured = seen.clone();
    let _subscription = emitter.on_replied(move |event| *captured.lock().expect("lock") = Some(event));

    emitter.emit_replied(PermissionRepliedEvent {
        request_id: "req-1".into(), session_id: "sess-1".into(), reply: PermissionDecision::Always,
    }).expect("emit");

    let event = seen.lock().expect("lock");
    let event = event.as_ref().expect("event");
    assert_eq!(event.request_id, "req-1");
    assert_eq!(event.session_id, "sess-1");
    assert_eq!(event.reply, PermissionDecision::Always);
}

#[test]
fn unsubscribe_asked_before_emit_removes_listener() {
    let emitter = PermissionEventEmitter::default();
    let count = Arc::new(Mutex::new(0));
    let captured = count.clone();
    let subscription = emitter.on_asked(move |_| *captured.lock().expect("lock") += 1);

    drop(subscription);
    emitter.emit_asked(&request()).expect("emit");

    assert_eq!(*count.lock().expect("lock"), 0);
}

#[test]
fn event_type_aliases_preserve_source_types() {
    let event: maho_ext_permission_system::events::PermissionAskedEvent = request();
    let reply = PermissionRepliedEvent {
        request_id: "req-1".into(), session_id: "sess-1".into(), reply: PermissionDecision::Always,
    };

    assert_eq!(event.id, "req-123");
    assert_eq!(reply.reply, PermissionDecision::Always);
}
