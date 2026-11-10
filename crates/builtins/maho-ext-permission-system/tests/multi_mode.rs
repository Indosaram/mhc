//! Port of senpi test/permission/multi-mode.test.ts: the service decisions every mode shares
//! (interactive, print, RPC, SDK with and without UI), the event payloads, mode transitions and the
//! opencode-ported `disabled()`, auto-respond and edge cases.
use maho_ext_api::BusSubscription;
use maho_ext_permission_system::{
    config::disabled,
    events::{PermissionEventEmitter, PermissionRepliedEvent},
    non_interactive::handle_no_ui,
    service::PermissionService,
    types::*,
};
use serde_json::{Map, Value, json};
use std::{
    future::Future,
    sync::{Arc, Mutex},
    task::{Context, Poll, Waker},
};

struct Service {
    service: PermissionService,
    asked: Arc<Mutex<Vec<Request>>>,
    replied: Arc<Mutex<Vec<PermissionRepliedEvent>>>,
    _subscriptions: [BusSubscription; 2],
}
fn service(static_rules: Ruleset, approved: Ruleset) -> Service {
    let emitter = PermissionEventEmitter::default();
    let asked = Arc::new(Mutex::new(Vec::new()));
    let replied = Arc::new(Mutex::new(Vec::new()));
    let (a, r) = (asked.clone(), replied.clone());
    let subscriptions = [
        emitter.on_asked(move |request| a.lock().expect("asked").push(request)),
        emitter.on_replied(move |event| r.lock().expect("replied").push(event)),
    ];
    Service {
        service: PermissionService::new(static_rules, approved, emitter),
        asked,
        replied,
        _subscriptions: subscriptions,
    }
}
impl Service {
    fn reply(&mut self, id: &str, reply: Reply, message: Option<&str>) {
        self.service.reply(ReplyInput {
            request_id: id.into(),
            reply,
            message: message.map(str::to_owned),
            always: None,
        });
    }
    fn replies(&self) -> Vec<PermissionDecision> {
        self.replied
            .lock()
            .expect("replied")
            .iter()
            .map(|event| event.reply)
            .collect()
    }
}
fn rule(permission: &str, pattern: &str, action: Action) -> Rule {
    Rule {
        permission: permission.into(),
        pattern: pattern.into(),
        action,
    }
}
fn request(id: &str, session: &str, permission: &str, patterns: &[&str]) -> Request {
    let mut metadata = Map::new();
    metadata.insert("command".into(), "git commit -m test".into());
    Request::new(
        id.into(),
        session.into(),
        permission.into(),
        patterns.iter().map(|p| (*p).into()).collect(),
        vec!["git *".into()],
        metadata,
    )
}
fn settled<F: Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(output) => output,
        Poll::Pending => panic!("permission call is still pending"),
    }
}

#[test]
fn interactive_mode_pends_unmatched_and_settles_static_rules() {
    let mut s = service(vec![], vec![]);
    let pending = s
        .service
        .ask(request("test-1", "session-1", "bash", &["git commit"]));
    assert_eq!(s.service.list().len(), 1);
    let asked = s.asked.lock().expect("asked").clone();
    assert_eq!(asked.len(), 1);
    assert_eq!(
        (
            asked[0].id.as_str(),
            asked[0].session_id.as_str(),
            asked[0].permission.as_str()
        ),
        ("test-1", "session-1", "bash")
    );
    assert_eq!(asked[0].patterns, ["git commit"]);
    s.reply("test-1", Reply::Once, None);
    settled(pending).expect("once");

    let mut allow = service(vec![rule("bash", "*", Action::Allow)], vec![]);
    settled(
        allow
            .service
            .ask(request("request-1", "session-1", "bash", &["git commit"])),
    )
    .expect("allowed");
    assert!(allow.asked.lock().expect("asked").is_empty());
    assert!(allow.service.list().is_empty());

    let mut deny = service(vec![rule("bash", "*", Action::Deny)], vec![]);
    assert!(matches!(
        settled(
            deny.service
                .ask(request("request-1", "session-1", "bash", &["git commit"]))
        ),
        Err(PermissionError::Denied(_))
    ));
}

#[test]
fn print_and_sdk_without_ui_refuse_with_the_flag_hint_and_emit_asked() {
    let emitter = PermissionEventEmitter::default();
    let events = Arc::new(Mutex::new(Vec::new()));
    let captured = events.clone();
    let _asked = emitter.bus.on(
        "permission_asked",
        Arc::new(move |value: &Value| captured.lock().expect("events").push(value.clone())),
    );
    let print_request = request("print-test-1", "session-1", "bash", &["rm -rf /"]);
    let outcome = handle_no_ui(&print_request, false, &emitter).expect("emit");
    assert_eq!(
        serde_json::to_value(&outcome).expect("json"),
        json!({"requestID":"print-test-1","reply":"reject","message":format!("Permission required for bash (rm -rf /), and there is no UI to ask, so the call did not run. {DENIAL_IS_FINAL} The user can allow it with --permission bash=allow.")})
    );
    assert_eq!(
        *events.lock().expect("events"),
        [serde_json::to_value(&print_request).expect("json")]
    );
    assert!(
        outcome
            .message
            .expect("message")
            .contains("--permission bash=allow")
    );

    let mut read = service(vec![rule("read", "*", Action::Allow)], vec![]);
    settled(
        read.service
            .ask(request("sdk-no-ui-2", "session-1", "read", &["file.txt"])),
    )
    .expect("rules permit");
    let mut deny = service(vec![rule("bash", "rm *", Action::Deny)], vec![]);
    assert!(matches!(
        settled(
            deny.service
                .ask(request("sdk-no-ui-3", "session-1", "bash", &["rm -rf /"]))
        ),
        Err(PermissionError::Denied(_))
    ));
}

#[test]
fn rpc_and_sdk_ui_replies_resolve_independently() {
    let mut s = service(vec![], vec![]);
    let pending = s
        .service
        .ask(request("rpc-test-1", "session-1", "bash", &["git commit"]));
    assert_eq!(
        (s.service.list().len(), s.asked.lock().expect("asked").len()),
        (1, 1)
    );
    s.reply("rpc-test-1", Reply::Always, None);
    settled(pending).expect("always");
    assert_eq!(
        serde_json::to_value(&*s.replied.lock().expect("replied")).expect("json"),
        json!([{"requestID":"rpc-test-1","sessionID":"session-1","reply":"always"}])
    );

    let mut s = service(vec![], vec![]);
    let first = s
        .service
        .ask(request("rpc-multi-1", "session-1", "bash", &["ls"]));
    let second = s
        .service
        .ask(request("rpc-multi-2", "session-1", "edit", &["file.ts"]));
    assert_eq!(s.service.list().len(), 2);
    s.reply("rpc-multi-1", Reply::Once, None);
    settled(first).expect("once");
    assert_eq!(s.service.list().len(), 1);
    s.reply("rpc-multi-2", Reply::Reject, None);
    assert!(matches!(settled(second), Err(PermissionError::Rejected)));

    let pending = s.service.ask(request(
        "sdk-ui-test-2",
        "session-1",
        "bash",
        &["git commit"],
    ));
    s.reply("sdk-ui-test-2", Reply::Reject, Some("Use a safer command"));
    let error = settled(pending).expect_err("corrected");
    assert!(
        matches!(&error, PermissionError::Corrected(feedback) if feedback == "Use a safer command")
    );
    assert!(error.to_string().contains("Use a safer command"));
}

#[test]
fn event_payloads_have_the_wire_shape_and_reach_every_handler() {
    let emitter = PermissionEventEmitter::default();
    let asked = Arc::new(Mutex::new(Vec::new()));
    let (first, second) = (asked.clone(), asked.clone());
    let _a = emitter.on_asked(move |request| {
        first
            .lock()
            .expect("asked")
            .push(serde_json::to_value(request).expect("json"))
    });
    let _b = emitter.on_asked(move |request| {
        second
            .lock()
            .expect("asked")
            .push(serde_json::to_value(request).expect("json"))
    });
    let mut full = request(
        "test-req-1",
        "test-sess-1",
        "bash",
        &["git commit", "git push"],
    );
    full.tool = Some(RequestTool {
        message_id: Some("msg-1".into()),
        call_id: "call-1".into(),
        parent_call_id: None,
    });
    emitter.emit_asked(&full).expect("emit");
    let expected = json!({"id":"test-req-1","sessionID":"test-sess-1","permission":"bash","patterns":["git commit","git push"],"always":["git *"],"metadata":{"command":"git commit -m test"},"tool":{"messageID":"msg-1","callID":"call-1"}});
    assert_eq!(*asked.lock().expect("asked"), [expected.clone(), expected]);

    let replied = Arc::new(Mutex::new(Vec::new()));
    let captured = replied.clone();
    let _r = emitter.on_replied(move |event| {
        captured
            .lock()
            .expect("replied")
            .push(serde_json::to_value(event).expect("json"))
    });
    for (id, session, reply, wire) in [
        ("req-123", "sess-456", PermissionDecision::Always, "always"),
        ("req-1", "sess-1", PermissionDecision::Once, "once"),
        ("req-1", "sess-1", PermissionDecision::Reject, "reject"),
    ] {
        emitter
            .emit_replied(PermissionRepliedEvent {
                request_id: id.into(),
                session_id: session.into(),
                reply,
            })
            .expect("emit");
        assert_eq!(
            replied.lock().expect("replied").pop(),
            Some(json!({"requestID":id,"sessionID":session,"reply":wire}))
        );
    }
}

#[test]
fn mode_transitions_once_asks_again_and_always_auto_allows() {
    let mut s = service(vec![], vec![]);
    let first = s
        .service
        .ask(request("seq-1", "session-1", "bash", &["ls"]));
    s.reply("seq-1", Reply::Once, None);
    settled(first).expect("once");
    let second = s
        .service
        .ask(request("seq-2", "session-1", "bash", &["pwd"]));
    assert_eq!(s.service.list().len(), 1);
    s.reply("seq-2", Reply::Reject, None);
    assert!(matches!(settled(second), Err(PermissionError::Rejected)));

    let mut s = service(vec![], vec![]);
    let first = s
        .service
        .ask(request("always-1", "session-1", "bash", &["git *"]));
    s.reply("always-1", Reply::Always, None);
    settled(first).expect("always");
    settled(
        s.service
            .ask(request("always-2", "session-1", "bash", &["git commit"])),
    )
    .expect("auto-allowed");
}

#[test]
fn disabled_matches_the_opencode_port() {
    let tools = |names: &[&str]| names.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>();
    let names = |set: std::collections::BTreeSet<String>| set.into_iter().collect::<Vec<_>>();
    let all = tools(&["bash", "edit", "read"]);
    assert!(disabled(&all, &[rule("*", "*", Action::Allow)]).is_empty());
    assert_eq!(
        names(disabled(
            &all,
            &[
                rule("*", "*", Action::Allow),
                rule("bash", "*", Action::Deny)
            ]
        )),
        ["bash"]
    );
    assert!(
        disabled(
            &tools(&["bash"]),
            &[
                rule("bash", "*", Action::Allow),
                rule("bash", "rm *", Action::Deny)
            ]
        )
        .is_empty()
    );
    assert!(disabled(&tools(&["bash", "edit"]), &[rule("*", "*", Action::Ask)]).is_empty());
    assert!(
        disabled(
            &tools(&["bash"]),
            &[
                rule("bash", "*", Action::Deny),
                rule("bash", "echo *", Action::Allow)
            ]
        )
        .is_empty()
    );
    assert_eq!(
        disabled(
            &all,
            &[
                rule("bash", "*", Action::Deny),
                rule("edit", "*", Action::Deny),
                rule("read", "*", Action::Deny)
            ]
        )
        .len(),
        3
    );
    assert_eq!(disabled(&all, &[rule("*", "*", Action::Deny)]).len(), 3);
    assert_eq!(
        names(disabled(
            &all,
            &[
                rule("*", "*", Action::Deny),
                rule("bash", "*", Action::Allow)
            ]
        )),
        ["edit", "read"]
    );
}

#[test]
fn auto_respond_patterns_follow_approved_and_configured_rules() {
    let mut accept = service(vec![], vec![rule("bash", "git *", Action::Allow)]);
    settled(
        accept
            .service
            .ask(request("request-1", "session-1", "bash", &["git commit"])),
    )
    .expect("auto-accept");
    let mut danger = service(vec![rule("bash", "rm -rf /", Action::Deny)], vec![]);
    assert!(matches!(
        settled(
            danger
                .service
                .ask(request("request-1", "session-1", "bash", &["rm -rf /"]))
        ),
        Err(PermissionError::Denied(_))
    ));
    let mut directory = service(
        vec![],
        vec![
            rule("edit", "/tmp/project/*", Action::Allow),
            rule("read", "/tmp/project/*", Action::Allow),
        ],
    );
    settled(directory.service.ask(request(
        "request-1",
        "session-1",
        "edit",
        &["/tmp/project/file.ts"],
    )))
    .expect("directory-scoped");
    let mut ordered = service(
        vec![
            rule("bash", "*", Action::Allow),
            rule("bash", "rm *", Action::Deny),
        ],
        vec![],
    );
    assert!(matches!(
        settled(
            ordered
                .service
                .ask(request("request-1", "session-1", "bash", &["rm -rf tmp"]))
        ),
        Err(PermissionError::Denied(_))
    ));
    settled(
        ordered
            .service
            .ask(request("request-2", "session-1", "bash", &["ls -la"])),
    )
    .expect("later rule order");
}

#[test]
fn edge_cases_from_next_test() {
    let mut s = service(vec![], vec![]);
    settled(
        s.service
            .ask(request("empty-patterns", "session-1", "bash", &[])),
    )
    .expect("nothing to check");
    let mut mixed = service(
        vec![
            rule("bash", "echo *", Action::Allow),
            rule("bash", "rm *", Action::Deny),
        ],
        vec![],
    );
    assert!(matches!(
        settled(mixed.service.ask(request(
            "request-1",
            "session-1",
            "bash",
            &["echo hello", "rm -rf /"]
        ))),
        Err(PermissionError::Denied(_))
    ));

    let mut s = service(vec![], vec![]);
    let first = s
        .service
        .ask(request("cascade-1", "sess-same", "bash", &["git commit"]));
    let second = s
        .service
        .ask(request("cascade-2", "sess-same", "edit", &["git commit"]));
    assert_eq!(s.service.list().len(), 2);
    s.reply("cascade-1", Reply::Reject, None);
    assert!(matches!(settled(first), Err(PermissionError::Rejected)));
    assert!(matches!(settled(second), Err(PermissionError::Rejected)));
    assert_eq!(
        s.replies(),
        [PermissionDecision::Reject, PermissionDecision::Reject]
    );

    let mut s = service(vec![], vec![]);
    let first = s.service.ask(request(
        "cascade-allow-1",
        "sess-same",
        "bash",
        &["git commit"],
    ));
    let second = s.service.ask(request(
        "cascade-allow-2",
        "sess-same",
        "bash",
        &["git push"],
    ));
    assert_eq!(s.service.list().len(), 2);
    s.reply("cascade-allow-1", Reply::Always, None);
    settled(first).expect("always");
    settled(second).expect("cascaded");
    assert_eq!(s.replies().len(), 2);

    let mut s = service(vec![], vec![]);
    let first = s
        .service
        .ask(request("diff-sess-1", "sess-a", "bash", &["git commit"]));
    let second = s
        .service
        .ask(request("diff-sess-2", "sess-b", "bash", &["git commit"]));
    s.reply("diff-sess-1", Reply::Reject, None);
    assert!(matches!(settled(first), Err(PermissionError::Rejected)));
    assert_eq!(
        s.service
            .list()
            .iter()
            .map(|r| r.id.as_str())
            .collect::<Vec<_>>(),
        ["diff-sess-2"]
    );
    s.reply("diff-sess-2", Reply::Reject, None);
    assert!(matches!(settled(second), Err(PermissionError::Rejected)));

    let mut quiet = service(vec![], vec![]);
    quiet.reply("unknown-id", Reply::Once, None);
    assert!(quiet.service.list().is_empty() && quiet.replies().is_empty());

    let pending = quiet.service.ask(request(
        "corrected-test",
        "session-1",
        "bash",
        &["git commit"],
    ));
    quiet.reply(
        "corrected-test",
        Reply::Reject,
        Some("Please use git status instead"),
    );
    let error = settled(pending).expect_err("corrected");
    assert!(
        matches!(&error, PermissionError::Corrected(feedback) if feedback == "Please use git status instead")
    );
    assert!(error.to_string().contains("Please use git status instead"));
}
