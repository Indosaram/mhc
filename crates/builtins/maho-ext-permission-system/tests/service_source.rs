use maho_ext_permission_system::{events::PermissionEventEmitter, service::PermissionService, types::*};
use std::sync::{Arc, Mutex};

fn request(id: &str) -> Request {
    Request { id: id.into(), session_id: "session".into(), permission: "bash".into(), patterns: vec!["git commit".into()], always: vec!["git *".into()], metadata: Default::default(), tool: None }
}
fn rule(pattern: &str, action: Action) -> Rule {
    Rule { permission: "bash".into(), pattern: pattern.into(), action }
}

#[tokio::test]
async fn empty_patterns_settle_without_pending_and_emit_allow() {
    let emitter = PermissionEventEmitter::default();
    let events = Arc::new(Mutex::new(Vec::new()));
    let captured = events.clone();
    let _asked = emitter.on_asked(move |event| captured.lock().expect("events").push(serde_json::to_value(event).expect("asked")));
    let captured = events.clone();
    let _replied = emitter.on_replied(move |event| captured.lock().expect("events").push(serde_json::to_value(event).expect("replied")));
    let mut service = PermissionService::new(vec![], vec![], emitter);
    let mut empty = request("empty");
    empty.patterns.clear();
    service.ask(empty).await.expect("nothing to check");
    assert!(service.list().is_empty());
    assert_eq!(*events.lock().expect("events"), vec![serde_json::json!({"requestID":"empty","sessionID":"session","reply":"allow"})]);
}

#[tokio::test]
async fn directory_approval_and_session_override_preserve_scope() {
    let directory = Rule { permission: "edit".into(), pattern: "/tmp/project/*".into(), action: Action::Allow };
    let mut service = PermissionService::new(vec![directory], vec![], PermissionEventEmitter::default());
    let mut edit = request("directory");
    edit.permission = "edit".into();
    edit.patterns = vec!["/tmp/project/file.ts".into()];
    service.ask(edit).await.expect("directory allowed");
    let mut service = PermissionService::new(vec![rule("*", Action::Allow)], vec![rule("rm *", Action::Deny)], PermissionEventEmitter::default());
    let mut denied = request("override");
    denied.patterns = vec!["rm -rf tmp".into()];
    assert!(matches!(service.ask(denied).await, Err(PermissionError::Denied(patterns)) if patterns == ["rm -rf tmp"]));
    let mut allowed = request("outside-override");
    allowed.patterns = vec!["ls -la".into()];
    service.ask(allowed).await.expect("base allow outside override");
    assert!(service.list().is_empty());
}

#[tokio::test]
async fn once_requires_new_reply_but_always_allows_next_matching_request() {
    for reply in [Reply::Once, Reply::Always] {
        let mut service = PermissionService::new(vec![], vec![], PermissionEventEmitter::default());
        let first = service.ask(request("first"));
        service.reply(ReplyInput { request_id: "first".into(), reply, message: None });
        first.await.expect("first approval");
        let second = service.ask(request("second"));
        let pending = service.list();
        if reply == Reply::Once {
            service.reply(ReplyInput { request_id: "second".into(), reply: Reply::Reject, message: None });
            assert!(matches!(second.await, Err(PermissionError::Rejected)));
            assert_eq!(serde_json::to_value(pending).expect("pending"), serde_json::to_value(vec![request("second")]).expect("request"));
            assert!(service.get_approved().is_empty());
        } else {
            second.await.expect("persistent approval");
            assert!(pending.is_empty());
            assert_eq!(service.get_approved(), vec![rule("git *", Action::Allow)]);
        }
        assert!(service.list().is_empty());
    }
}

#[tokio::test]
async fn auto_allow_emits_complete_reply_without_pending() {
    for approved in [false, true] {
        // Given a listener subscribed before static or persisted admission.
        let emitter = PermissionEventEmitter::default();
        let events = Arc::new(Mutex::new(Vec::new()));
        let captured = events.clone();
        let _subscription = emitter.on_replied(move |event| captured.lock().expect("events").push(event));
        let rules = vec![rule("*", Action::Allow)];
        let mut service = PermissionService::new(if approved { vec![] } else { rules.clone() }, if approved { rules } else { vec![] }, emitter);
        // When an allowed request arrives.
        service.ask(request("first")).await.expect("allowed");
        // Then the source event contains the request, session and decision.
        assert!(service.list().is_empty());
        let events = events.lock().expect("events");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].request_id, "first");
        assert_eq!(events[0].session_id, "session");
        assert_eq!(events[0].reply, PermissionDecision::Allow);
    }
}

#[tokio::test]
async fn mixed_allowed_pending_request_emits_original_payload_and_once_reply() {
    // Given an allowed pattern plus another requiring confirmation.
    let emitter = PermissionEventEmitter::default();
    let asked = Arc::new(Mutex::new(Vec::new()));
    let captured = asked.clone();
    let _asked = emitter.on_asked(move |event| captured.lock().expect("asked").push(event));
    let replied = Arc::new(Mutex::new(Vec::new()));
    let captured = replied.clone();
    let _replied = emitter.on_replied(move |event| captured.lock().expect("replied").push(event));
    let mut service = PermissionService::new(vec![rule("git status", Action::Allow)], vec![], emitter);
    let mut original = request("mixed");
    original.patterns = vec!["git status".into(), "git commit".into()];
    // When admission waits and the user approves once.
    let completion = service.ask(original.clone());
    assert_eq!(serde_json::to_value(service.list()).expect("pending payload"), serde_json::to_value([original.clone()]).expect("original payload"));
    service.reply(ReplyInput { request_id: "mixed".into(), reply: Reply::Once, message: None });
    completion.await.expect("once");
    // Then complete asked payload and reply survive, with no persistent approval.
    assert_eq!(serde_json::to_value(&*asked.lock().expect("asked")).expect("asked payload"), serde_json::to_value([original]).expect("original payload"));
    assert!(service.list().is_empty());
    assert!(service.get_approved().is_empty());
    let events = replied.lock().expect("replied");
    assert_eq!(events[0].request_id, "mixed");
    assert_eq!(events[0].session_id, "session");
    assert_eq!(events[0].reply, PermissionDecision::Once);
}

#[tokio::test]
async fn always_preserves_existing_approval_and_reevaluates_all_patterns() {
    // Given existing approval and two same-session pending requests.
    let emitter = PermissionEventEmitter::default();
    let events = Arc::new(Mutex::new(Vec::new()));
    let captured = events.clone();
    let _subscription = emitter.on_replied(move |event| captured.lock().expect("events").push(event));
    let mut service = PermissionService::new(vec![rule("git status", Action::Allow), rule("*", Action::Ask)], vec![rule("gh *", Action::Allow)], emitter);
    let mut first = request("first");
    first.always = vec!["git *".into(), "npm *".into()];
    let first_completion = service.ask(first);
    let mut second = request("second");
    second.patterns = vec!["git status".into(), "git push".into()];
    let second_completion = service.ask(second);
    // When granting persistent approval.
    service.reply(ReplyInput { request_id: "first".into(), reply: Reply::Always, message: None });
    // Then both settle, all new patterns append and prior approvals remain.
    first_completion.await.expect("first");
    second_completion.await.expect("second");
    assert!(service.list().is_empty());
    assert_eq!(service.get_approved(), [rule("gh *", Action::Allow), rule("git *", Action::Allow), rule("npm *", Action::Allow)]);
    let events = events.lock().expect("events");
    assert_eq!(events.iter().map(|event| event.request_id.as_str()).collect::<Vec<_>>(), ["first", "second"]);
    assert!(events.iter().all(|event| event.reply == PermissionDecision::Always && event.session_id == "session"));
}

#[tokio::test]
async fn corrected_rejection_emits_reject_for_original_and_cascade() {
    // Given two pending requests and a subscribed reply listener.
    let emitter = PermissionEventEmitter::default();
    let events = Arc::new(Mutex::new(Vec::new()));
    let captured = events.clone();
    let _subscription = emitter.on_replied(move |event| captured.lock().expect("events").push(event));
    let mut service = PermissionService::new(vec![], vec![], emitter);
    let first = service.ask(request("first"));
    let second = service.ask(request("second"));
    // When rejecting the original with feedback.
    service.reply(ReplyInput { request_id: "first".into(), reply: Reply::Reject, message: Some("use status".into()) });
    // Then only the original carries feedback and both emit rejection.
    assert!(matches!(first.await, Err(PermissionError::Corrected(message)) if message == "use status"));
    assert!(matches!(second.await, Err(PermissionError::Rejected)));
    assert!(service.list().is_empty());
    let events = events.lock().expect("events");
    assert_eq!(events.iter().map(|event| event.request_id.as_str()).collect::<Vec<_>>(), ["first", "second"]);
    assert!(events.iter().all(|event| event.reply == PermissionDecision::Reject && event.session_id == "session"));
}
