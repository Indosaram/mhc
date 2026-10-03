use maho_ext_permission_system::{events::PermissionEventEmitter, service::PermissionService, types::*};
use std::sync::{Arc, Mutex};

fn request(id: &str) -> Request {
    Request { id: id.into(), session_id: "session".into(), permission: "bash".into(), patterns: vec!["git commit".into()], always: vec!["git *".into()], metadata: Default::default(), tool: None }
}
fn rule(pattern: &str, action: Action) -> Rule {
    Rule { permission: "bash".into(), pattern: pattern.into(), action }
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
