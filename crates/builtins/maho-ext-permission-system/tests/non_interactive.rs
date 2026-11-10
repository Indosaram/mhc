//! Port of the no-UI rows of test/permission/multi-mode.test.ts, integration.test.ts and
//! presets.test.ts: `handle_no_ui` only answers a request the service is still asking about, so it
//! always refuses.
use maho_ext_permission_system::{
    events::PermissionEventEmitter,
    non_interactive::handle_no_ui,
    parsers::create_builtin_parser_registry,
    service::{AskOptions, PermissionService},
    types::{Action, DENIAL_IS_FINAL, PermissionDecision, PermissionError, Reply, Request, Rule},
};
use serde_json::json;
use std::{
    future::Future,
    path::Path,
    sync::{Arc, Mutex},
};

fn request(permission: &str, patterns: &[&str]) -> Request {
    Request {
        id: "print-test".into(),
        session_id: "session-1".into(),
        permission: permission.into(),
        patterns: patterns.iter().map(|pattern| (*pattern).into()).collect(),
        always: vec![],
        original_paths: None,
        metadata: Default::default(),
        wider_always: None,
        tool: None,
    }
}
fn rule(permission: &str, pattern: &str, action: Action) -> Rule {
    Rule {
        permission: permission.into(),
        pattern: pattern.into(),
        action,
    }
}

#[test]
fn unmatched_headless_request_rejects_and_emits_only_asked() {
    let emitter = PermissionEventEmitter::default();
    let events = Arc::new(Mutex::new(Vec::new()));
    let asked = events.clone();
    let replied = events.clone();
    let _asked = emitter.bus.on(
        "permission_asked",
        Arc::new(move |value| asked.lock().expect("lock").push(value.clone())),
    );
    let _replied = emitter.bus.on(
        "permission_replied",
        Arc::new(move |value| replied.lock().expect("lock").push(value.clone())),
    );
    let request = request("bash", &["rm -rf /"]);

    let outcome = handle_no_ui(&request, false, &emitter).expect("emit");

    assert_eq!(outcome.request_id, request.id);
    assert_eq!(outcome.reply, Reply::Reject);
    assert_eq!(
        outcome.message.as_deref(),
        Some(format!("Permission required for bash (rm -rf /), and there is no UI to ask, so the call did not run. {DENIAL_IS_FINAL} The user can allow it with --permission bash=allow.").as_str())
    );
    assert_eq!(
        *events.lock().expect("lock"),
        vec![serde_json::to_value(request).expect("json")]
    );
}

#[test]
fn every_pattern_is_named_in_the_refusal() {
    let outcome = handle_no_ui(
        &request("bash", &["ls", "pwd"]),
        false,
        &PermissionEventEmitter::default(),
    )
    .expect("emit");
    assert_eq!(
        outcome.message.as_deref(),
        Some(format!("Permission required for bash (ls, pwd), and there is no UI to ask, so the call did not run. {DENIAL_IS_FINAL} The user can allow it with --permission bash=allow.").as_str())
    );
}

#[test]
fn auto_preset_refusal_says_allow_rules_do_not_widen_it() {
    let outcome = handle_no_ui(
        &request("bash", &["git status"]),
        true,
        &PermissionEventEmitter::default(),
    )
    .expect("emit");
    assert_eq!(outcome.reply, Reply::Reject);
    assert_eq!(
        outcome.message.as_deref(),
        Some(format!("Permission required for bash (git status), and there is no UI to ask, so the call did not run. {DENIAL_IS_FINAL} Under the auto preset, allow rules do not widen what it approves; the user can run it interactively or choose another preset.").as_str())
    );
}

/// A multi-command bash call whose first command is allowed and a later one asks: the service keeps
/// it pending, and the no-UI answer must settle it as a refusal. It must never emit an allow or leave
/// the request pending (the old first-pattern re-check returned no reply, so the call hung).
#[test]
fn first_command_allowed_later_command_asks_is_refused_and_settled() {
    for command in ["cd src && rm -rf build", "rm -rf build; cd src"] {
        let cwd = tempfile::tempdir().expect("cwd");
        let parsed = create_builtin_parser_registry().parse(
            "bash",
            &json!({ "command": command }),
            (cwd.path(), Path::new("/home/u")),
        );
        let bash = parsed
            .into_iter()
            .find(|request| request.permission == "bash")
            .expect("bash request");
        assert_eq!(bash.patterns.len(), 2, "{command}: {:?}", bash.patterns);

        let rules = vec![
            rule("bash", "*", Action::Ask),
            rule("bash", "cd *", Action::Allow),
        ];
        let service_emitter = PermissionEventEmitter::default();
        let replies = Arc::new(Mutex::new(Vec::new()));
        let captured = replies.clone();
        let _replied = service_emitter
            .on_replied(move |event| captured.lock().expect("replies").push(event.reply));
        let mut service = PermissionService::new(rules, vec![], service_emitter);
        let mut request = request("bash", &[]);
        request.patterns = bash.patterns;
        let settled = service.ask_with(request.clone(), AskOptions::default());
        assert_eq!(service.list().len(), 1, "{command}: the service still asks");

        let reply =
            handle_no_ui(&request, false, &PermissionEventEmitter::default()).expect("emit");
        service.reply(reply);

        assert!(service.list().is_empty(), "{command}: nothing left pending");
        // The reply settles the call synchronously: one poll must be Ready (a hang would be Pending).
        let mut settled = std::pin::pin!(settled);
        let std::task::Poll::Ready(result) = settled
            .as_mut()
            .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
        else {
            panic!("{command}: the call is still pending after the no-UI reply");
        };
        assert!(
            matches!(result, Err(PermissionError::Corrected(ref message)) if message.contains(DENIAL_IS_FINAL)),
            "{command}: {result:?}"
        );
        assert!(
            replies
                .lock()
                .expect("replies")
                .iter()
                .all(|reply| *reply != PermissionDecision::Allow),
            "{command}: no allow is emitted"
        );
    }
}
