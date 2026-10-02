use maho_ext_permission_system::{events::PermissionEventEmitter, non_interactive::handle_no_ui, types::{Action, Reply, Request, Rule}};
use std::sync::{Arc, Mutex};

fn request(permission: &str, pattern: &str) -> Request {
    Request { id: "print-test".into(), session_id: "session-1".into(), permission: permission.into(), patterns: vec![pattern.into()], always: vec![], metadata: Default::default(), tool: None }
}
fn rule(permission: &str, pattern: &str, action: Action) -> Rule {
    Rule { permission: permission.into(), pattern: pattern.into(), action }
}

#[test]
fn unmatched_headless_request_rejects_and_emits_only_asked() {
    let emitter = PermissionEventEmitter::default();
    let events = Arc::new(Mutex::new(Vec::new()));
    let asked = events.clone();
    let replied = events.clone();
    let _asked = emitter.bus.on("permission_asked", Arc::new(move |value| asked.lock().expect("lock").push(value.clone())));
    let _replied = emitter.bus.on("permission_replied", Arc::new(move |value| replied.lock().expect("lock").push(value.clone())));
    let request = request("bash", "rm -rf /");

    let outcome = handle_no_ui(&request, (&[], &[]), &emitter).expect("emit").expect("rejection");

    assert_eq!(outcome.request_id, request.id);
    assert_eq!(outcome.reply, Reply::Reject);
    assert_eq!(*events.lock().expect("lock"), vec![serde_json::to_value(request).expect("json")]);
}

#[test]
fn cli_allow_overrides_static_deny_and_emits_allow_decision() {
    let emitter = PermissionEventEmitter::default();
    let events = Arc::new(Mutex::new(Vec::new()));
    let capture = events.clone();
    let _subscription = emitter.bus.on("permission_replied", Arc::new(move |value| capture.lock().expect("lock").push(value.clone())));
    let static_rules = [rule("bash", "*", Action::Deny)];
    let cli_rules = [rule("bash", "*", Action::Allow)];

    let result = handle_no_ui(&request("bash", "ls"), (&static_rules, &cli_rules), &emitter).expect("emit");

    assert!(result.is_none());
    assert_eq!(*events.lock().expect("lock"), vec![serde_json::json!({"requestID":"print-test","sessionID":"session-1","reply":"allow"})]);
}

#[test]
fn cli_deny_overrides_static_allow() {
    let emitter = PermissionEventEmitter::default();
    let static_rules = [rule("bash", "*", Action::Allow)];
    let cli_rules = [rule("bash", "rm *", Action::Deny)];

    let result = handle_no_ui(&request("bash", "rm file"), (&static_rules, &cli_rules), &emitter).expect("emit").expect("reject");

    assert_eq!(result.reply, Reply::Reject);
}

#[test]
fn static_allow_permits_without_cli_override() {
    let emitter = PermissionEventEmitter::default();
    let static_rules = [rule("edit", "src/*", Action::Allow)];

    let result = handle_no_ui(&request("edit", "src/*.ts"), (&static_rules, &[]), &emitter).expect("emit");

    assert!(result.is_none());
}

#[test]
fn static_deny_rejects_without_cli_override() {
    let emitter = PermissionEventEmitter::default();
    let static_rules = [rule("edit", "node_modules/*", Action::Deny)];

    let result = handle_no_ui(&request("edit", "node_modules/*"), (&static_rules, &[]), &emitter).expect("emit").expect("reject");

    assert_eq!(result.reply, Reply::Reject);
}
