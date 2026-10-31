//! Port of senpi test/permission/integration.test.ts (sections 1-15 and the additional scenarios)
//! against service.rs, parsers.rs, config.rs, cli.rs, storage.rs, evaluate.rs and non_interactive.rs.
use maho_ext_api::BusSubscription;
use maho_ext_permission_system::{
    cli::parse_permission_flag,
    config::{disabled, expand, from_config, merge},
    evaluate::evaluate,
    events::{PermissionEventEmitter, PermissionRepliedEvent},
    non_interactive::handle_no_ui,
    parsers::create_builtin_parser_registry,
    service::PermissionService,
    storage::{append_approved, clear_approved, load_approved},
    types::*,
};
use serde_json::{Map, Value, json};
use std::{
    future::Future,
    path::Path,
    sync::{Arc, Mutex},
    task::{Context, Poll, Waker},
};

struct Harness {
    service: PermissionService,
    asked: Arc<Mutex<Vec<Request>>>,
    replied: Arc<Mutex<Vec<PermissionRepliedEvent>>>,
    _subscriptions: [BusSubscription; 2],
}
fn harness(static_rules: Ruleset, approved: Ruleset) -> Harness {
    let emitter = PermissionEventEmitter::default();
    let asked = Arc::new(Mutex::new(Vec::new()));
    let replied = Arc::new(Mutex::new(Vec::new()));
    let (a, r) = (asked.clone(), replied.clone());
    let subscriptions = [
        emitter.on_asked(move |request| a.lock().expect("asked").push(request)),
        emitter.on_replied(move |event| r.lock().expect("replied").push(event)),
    ];
    Harness {
        service: PermissionService::new(static_rules, approved, emitter),
        asked,
        replied,
        _subscriptions: subscriptions,
    }
}
impl Harness {
    fn asked(&self) -> usize {
        self.asked.lock().expect("asked").len()
    }
    fn replied(&self) -> Vec<Value> {
        self.replied
            .lock()
            .expect("replied")
            .iter()
            .map(|event| serde_json::to_value(event).expect("json"))
            .collect()
    }
    fn reply(&mut self, id: &str, reply: Reply, message: Option<&str>) {
        self.service.reply(ReplyInput {
            request_id: id.into(),
            reply,
            message: message.map(str::to_owned),
            always: None,
        });
    }
}
fn rule(permission: &str, pattern: &str, action: Action) -> Rule {
    Rule {
        permission: permission.into(),
        pattern: pattern.into(),
        action,
    }
}
fn request(
    id: &str,
    session: &str,
    permission: &str,
    patterns: &[&str],
    always: &[&str],
) -> Request {
    let mut metadata = Map::new();
    metadata.insert("command".into(), "git commit -m test".into());
    Request::new(
        id.into(),
        session.into(),
        permission.into(),
        patterns.iter().map(|p| (*p).into()).collect(),
        always.iter().map(|p| (*p).into()).collect(),
        metadata,
    )
}
fn bash(id: &str, patterns: &[&str]) -> Request {
    request(id, "session-1", "bash", patterns, &["git *"])
}
/// A settled call answers on its first poll; a still-pending call is a hang.
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
fn parse(
    tool: &str,
    input: Value,
    cwd: &Path,
) -> Vec<maho_ext_permission_system::parsers::PermissionRequest> {
    create_builtin_parser_registry().parse(tool, &input, (cwd, Path::new("/home/user")))
}

// 1. allow-all config -> no prompt, tool executes
#[test]
fn allow_all_config_runs_without_prompt() {
    let mut h = harness(vec![rule("bash", "*", Action::Allow)], vec![]);
    settled(h.service.ask(bash("request-1", &["ls -la"]))).expect("allowed");
    let mut edit = harness(vec![rule("edit", "*", Action::Allow)], vec![]);
    settled(edit.service.ask(request(
        "request-1",
        "session-1",
        "edit",
        &["src/index.ts"],
        &["src/index.ts"],
    )))
    .expect("edit allowed");
    settled(h.service.ask(request(
        "request-2",
        "session-1",
        "bash",
        &["echo hello", "ls -la", "pwd"],
        &["*"],
    )))
    .expect("all allowed");
    assert_eq!((h.asked(), edit.asked()), (0, 0));
    assert!(h.service.list().is_empty());
}

// 2. deny-all config -> DeniedError
#[test]
fn deny_config_blocks_with_denied_patterns() {
    let mut h = harness(vec![rule("bash", "*", Action::Deny)], vec![]);
    assert!(matches!(
        settled(h.service.ask(bash("request-1", &["rm -rf /"]))),
        Err(PermissionError::Denied(_))
    ));
    assert_eq!(h.asked(), 0);
    let mut h = harness(vec![rule("bash", "rm *", Action::Deny)], vec![]);
    assert!(
        matches!(settled(h.service.ask(bash("request-1", &["rm -rf tmp"]))), Err(PermissionError::Denied(p)) if p == ["rm -rf tmp"])
    );
    let mut h = harness(
        vec![
            rule("bash", "*", Action::Allow),
            rule("bash", "rm *", Action::Deny),
        ],
        vec![],
    );
    assert!(matches!(
        settled(
            h.service
                .ask(bash("request-1", &["echo hello", "rm -rf tmp", "ls"]))
        ),
        Err(PermissionError::Denied(_))
    ));
}

// 3. ask config -> pending, reply once -> runs
#[test]
fn ask_config_pends_until_once() {
    let mut h = harness(vec![rule("bash", "*", Action::Ask)], vec![]);
    let pending = h.service.ask(bash("req-1", &["git commit"]));
    assert_eq!(h.service.list().len(), 1);
    assert_eq!(h.asked.lock().expect("asked")[0].id, "req-1");
    h.reply("req-1", Reply::Once, None);
    settled(pending).expect("once");
    assert!(h.service.list().is_empty());
    let pending = h.service.ask(request(
        "req-3",
        "sess-1",
        "bash",
        &["git commit"],
        &["git *"],
    ));
    h.replied.lock().expect("replied").clear();
    h.reply("req-3", Reply::Once, None);
    settled(pending).expect("once");
    assert_eq!(
        h.replied(),
        vec![json!({"requestID":"req-3","sessionID":"sess-1","reply":"once"})]
    );
}

// 4. ask config -> always -> rule stored, later calls auto-allowed
#[test]
fn always_stores_rules_and_auto_allows_later_calls() {
    let mut h = harness(vec![rule("bash", "*", Action::Ask)], vec![]);
    let pending = h.service.ask(bash("req-4", &["git commit"]));
    h.reply("req-4", Reply::Always, None);
    settled(pending).expect("always");
    assert_eq!(
        h.service.get_approved(),
        vec![rule("bash", "git *", Action::Allow)]
    );
    h.asked.lock().expect("asked").clear();
    settled(h.service.ask(bash("req-second", &["git push"]))).expect("auto-allowed");
    assert_eq!(h.asked(), 0);
    assert!(h.service.list().is_empty());

    let mut h = harness(vec![rule("bash", "*", Action::Ask)], vec![]);
    let pending = h.service.ask(request(
        "req-5",
        "session-1",
        "bash",
        &["git commit"],
        &["git *", "gh *"],
    ));
    h.reply("req-5", Reply::Always, None);
    settled(pending).expect("always");
    assert_eq!(
        h.service.get_approved(),
        vec![
            rule("bash", "git *", Action::Allow),
            rule("bash", "gh *", Action::Allow)
        ]
    );
}

// 5. reject -> RejectedError; 6. reject with feedback -> CorrectedError
#[test]
fn reject_and_feedback_settle_as_rejected_or_corrected() {
    let mut h = harness(vec![rule("bash", "*", Action::Ask)], vec![]);
    let pending = h.service.ask(request(
        "req-8",
        "sess-2",
        "bash",
        &["git commit"],
        &["git *"],
    ));
    h.reply("req-8", Reply::Reject, None);
    assert!(matches!(settled(pending), Err(PermissionError::Rejected)));
    assert!(h.service.list().is_empty());
    assert_eq!(
        h.replied(),
        vec![json!({"requestID":"req-8","sessionID":"sess-2","reply":"reject"})]
    );
    for feedback in [
        "Use git status instead",
        "This is dangerous, use rm -i instead",
    ] {
        let pending = h.service.ask(bash("req-9", &["git commit"]));
        h.reply("req-9", Reply::Reject, Some(feedback));
        let error = settled(pending).expect_err("corrected");
        assert!(matches!(&error, PermissionError::Corrected(text) if text == feedback));
        assert!(error.to_string().contains(feedback));
    }
}

// 7. cascade reject
#[test]
fn reject_cascades_to_the_same_session_only() {
    let mut h = harness(vec![rule("bash", "*", Action::Ask)], vec![]);
    let first = h.service.ask(request(
        "req-c",
        "session-cascade",
        "bash",
        &["ls"],
        &["git *"],
    ));
    let second = h.service.ask(request(
        "req-d",
        "session-cascade",
        "bash",
        &["pwd"],
        &["git *"],
    ));
    h.reply("req-c", Reply::Reject, None);
    assert!(matches!(settled(first), Err(PermissionError::Rejected)));
    assert!(matches!(settled(second), Err(PermissionError::Rejected)));
    assert!(h.service.list().is_empty());
    assert_eq!(
        h.replied(),
        vec![
            json!({"requestID":"req-c","sessionID":"session-cascade","reply":"reject"}),
            json!({"requestID":"req-d","sessionID":"session-cascade","reply":"reject"}),
        ]
    );
    let first = h.service.ask(request(
        "req-sess-1",
        "session-1",
        "bash",
        &["ls"],
        &["git *"],
    ));
    let second = h.service.ask(request(
        "req-sess-2",
        "session-2",
        "bash",
        &["pwd"],
        &["git *"],
    ));
    h.reply("req-sess-1", Reply::Reject, None);
    assert!(matches!(settled(first), Err(PermissionError::Rejected)));
    assert_eq!(
        h.service
            .list()
            .iter()
            .map(|r| r.session_id.as_str())
            .collect::<Vec<_>>(),
        ["session-2"]
    );
    h.reply("req-sess-2", Reply::Once, None);
    settled(second).expect("once");
}

// 8. cascade allow
#[test]
fn always_resolves_only_covered_pending_in_the_same_session() {
    let mut h = harness(vec![rule("bash", "*", Action::Ask)], vec![]);
    let first = h.service.ask(request(
        "req-e",
        "session-auto",
        "bash",
        &["git commit"],
        &["git *"],
    ));
    let second = h.service.ask(request(
        "req-f",
        "session-auto",
        "bash",
        &["git push"],
        &["git *"],
    ));
    h.reply("req-e", Reply::Always, None);
    settled(first).expect("always");
    settled(second).expect("cascaded");
    assert!(h.service.list().is_empty());

    for (session, patterns, always) in [
        ("session-partial", "docker build", "docker *"),
        ("session-2", "git push", "git *"),
    ] {
        let mut h = harness(vec![rule("bash", "*", Action::Ask)], vec![]);
        let first = h.service.ask(request(
            "req-g",
            "session-partial",
            "bash",
            &["git commit"],
            &["git *"],
        ));
        let second = h
            .service
            .ask(request("req-h", session, "bash", &[patterns], &[always]));
        h.reply("req-g", Reply::Always, None);
        settled(first).expect("always");
        assert_eq!(
            h.service
                .list()
                .iter()
                .map(|r| r.id.as_str())
                .collect::<Vec<_>>(),
            ["req-h"]
        );
        h.reply("req-h", Reply::Once, None);
        settled(second).expect("once");
    }
}

// 9. bash arity
#[test]
fn bash_arity_prefixes_and_git_rule() {
    let cwd = Path::new("/project");
    for (command, prefix) in [
        ("git commit -m test", "git commit"),
        ("npm install lodash", "npm install"),
        ("ls -la", "ls"),
    ] {
        let requests = parse("bash", json!({ "command": command }), cwd);
        assert_eq!(requests.len(), 1, "{command}");
        assert_eq!(requests[0].permission, "bash");
        assert_eq!(requests[0].patterns, [prefix]);
        assert_eq!(
            requests[0].always,
            [prefix.to_owned(), format!("{prefix} *")]
        );
    }
    let mut h = harness(vec![rule("bash", "git *", Action::Allow)], vec![]);
    settled(h.service.ask(request(
        "request-1",
        "session-1",
        "bash",
        &["git commit"],
        &["git commit", "git commit *"],
    )))
    .expect("git * allows");
}

// 10. edit unification
#[test]
fn edit_tools_share_the_edit_permission() {
    let cwd = Path::new("/project");
    let write = parse("write", json!({"path":"src/index.ts"}), cwd);
    assert_eq!(write.len(), 1);
    assert_eq!(
        (write[0].permission.as_str(), write[0].patterns.clone()),
        ("edit", vec!["src/index.ts".to_owned()])
    );
    assert_eq!(
        parse("apply_patch", json!({"file_path":"src/utils.ts"}), cwd)[0].permission,
        "edit"
    );
    assert_eq!(
        parse("multiedit", json!({"path":"README.md"}), cwd)[0].permission,
        "edit"
    );
    let mut h = harness(vec![rule("edit", "*", Action::Allow)], vec![]);
    settled(h.service.ask(request(
        "request-1",
        "session-1",
        "edit",
        &["src/index.ts"],
        &["src/index.ts"],
    )))
    .expect("edit allowed");
    let tools: Vec<String> = ["edit", "write", "apply_patch", "multiedit", "read"]
        .map(str::to_owned)
        .into();
    let off = disabled(&tools, &[rule("edit", "*", Action::Deny)]);
    assert_eq!(
        off.into_iter().collect::<Vec<_>>(),
        ["apply_patch", "edit", "multiedit", "write"]
    );
}

// 11. external directory from bash
#[test]
fn bash_external_paths_request_external_directory() {
    let root = tempfile::tempdir().expect("tmp");
    let base = root.path().canonicalize().expect("real tmp");
    let cwd = base.join("work").join("project");
    std::fs::create_dir_all(&cwd).expect("cwd");
    let parent = cwd.parent().expect("parent");
    let requests = parse("bash", json!({"command":"cat ../config.txt"}), &cwd);
    assert_eq!(
        requests
            .iter()
            .map(|r| r.permission.as_str())
            .collect::<Vec<_>>(),
        ["bash", "external_directory"]
    );
    assert_eq!(
        requests[1].patterns,
        [parent.join("config.txt").to_string_lossy()]
    );
    assert_eq!(
        requests[1].original_paths.as_deref(),
        Some(&["../config.txt".to_owned()][..])
    );

    let requests = parse(
        "bash",
        json!({"command":"cp ../config.txt ../../backup/"}),
        &cwd,
    );
    let external = requests
        .iter()
        .find(|r| r.permission == "external_directory")
        .expect("external");
    assert_eq!(
        external.patterns,
        [
            parent.join("config.txt").to_string_lossy().into_owned(),
            base.join("backup").to_string_lossy().into_owned()
        ]
    );
    assert_eq!(
        external.original_paths.as_deref(),
        Some(&["../config.txt".to_owned(), "../../backup/".to_owned()][..])
    );

    let internal = create_builtin_parser_registry().parse(
        "bash",
        &json!({"command":"cat ./src/index.ts"}),
        (Path::new("/home/user/project"), Path::new("/home/user")),
    );
    assert_eq!(
        internal
            .iter()
            .map(|r| r.permission.as_str())
            .collect::<Vec<_>>(),
        ["bash"]
    );

    let home = base.join("home");
    std::fs::create_dir_all(&home).expect("home");
    let requests = create_builtin_parser_registry().parse(
        "bash",
        &json!({"command":"cat ~/permission-home-fixture.txt"}),
        (cwd.as_path(), home.as_path()),
    );
    let external = requests
        .iter()
        .find(|r| r.permission == "external_directory")
        .expect("external");
    assert_eq!(
        external.patterns,
        [home.join("permission-home-fixture.txt").to_string_lossy()]
    );
    assert_eq!(
        external.original_paths.as_deref(),
        Some(&["~/permission-home-fixture.txt".to_owned()][..])
    );
}

// 12. disabled()
#[test]
fn disabled_removes_only_wholly_denied_tools() {
    let tools = |names: &[&str]| names.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>();
    assert!(
        disabled(
            &tools(&["read", "write", "bash"]),
            &[
                rule("read", "*", Action::Allow),
                rule("write", "*", Action::Ask)
            ]
        )
        .is_empty()
    );
    assert_eq!(
        disabled(
            &tools(&["read", "bash", "grep"]),
            &[rule("bash", "*", Action::Deny)]
        )
        .into_iter()
        .collect::<Vec<_>>(),
        ["bash"]
    );
    assert_eq!(
        disabled(
            &tools(&["read", "write", "bash"]),
            &[rule("*", "*", Action::Deny)]
        )
        .len(),
        3
    );
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
}

// 13. settings config -> from_config
#[test]
fn from_config_expands_flat_nested_and_home_values() {
    let home = "/home/tester";
    let config = |value: Value| -> PermissionConfig {
        value
            .as_object()
            .expect("object")
            .iter()
            .map(|(k, v)| (k.clone(), serde_json::from_value(v.clone()).expect("value")))
            .collect()
    };
    assert_eq!(
        from_config(&config(json!({"bash":"allow","edit":"ask"})), home).expect("rules"),
        [
            rule("bash", "*", Action::Allow),
            rule("edit", "*", Action::Ask)
        ]
    );
    assert_eq!(
        from_config(
            &config(json!({"bash":{"*":"ask","git *":"allow","rm *":"deny"}})),
            home
        )
        .expect("rules"),
        [
            rule("bash", "*", Action::Ask),
            rule("bash", "git *", Action::Allow),
            rule("bash", "rm *", Action::Deny)
        ]
    );
    assert_eq!(
        from_config(
            &config(json!({"external_directory":{"~/projects/*":"allow"}})),
            home
        )
        .expect("rules"),
        [rule(
            "external_directory",
            "/home/tester/projects/*",
            Action::Allow
        )]
    );
    assert_eq!(
        from_config(
            &config(json!({"external_directory":{"$HOME/.config/*":"allow"}})),
            home
        )
        .expect("rules"),
        [rule(
            "external_directory",
            "/home/tester/.config/*",
            Action::Allow
        )]
    );
    assert_eq!(
        from_config(&config(json!({"read":"allow","write":{"~/projects/*":"ask"},"bash":{"*":"ask","git *":"allow"}})), home).expect("rules"),
        [rule("read", "*", Action::Allow), rule("write", "/home/tester/projects/*", Action::Ask), rule("bash", "*", Action::Ask), rule("bash", "git *", Action::Allow)]
    );
}

// 14. CLI flag
#[test]
fn cli_flag_parses_and_later_configured_allow_wins() {
    assert_eq!(
        parse_permission_flag("bash=allow"),
        [rule("bash", "*", Action::Allow)]
    );
    assert_eq!(
        parse_permission_flag("bash:git *=allow"),
        [rule("bash", "git *", Action::Allow)]
    );
    assert_eq!(
        parse_permission_flag("bash=allow,edit=ask,read=deny"),
        [
            rule("bash", "*", Action::Allow),
            rule("edit", "*", Action::Ask),
            rule("read", "*", Action::Deny)
        ]
    );
    assert_eq!(
        parse_permission_flag(" bash = allow , edit = ask "),
        [
            rule("bash", "*", Action::Allow),
            rule("edit", "*", Action::Ask)
        ]
    );
    let mut h = harness(
        vec![
            rule("bash", "*", Action::Deny),
            rule("bash", "*", Action::Allow),
        ],
        vec![],
    );
    settled(
        h.service
            .ask(request("request-1", "session-1", "bash", &["ls"], &["*"])),
    )
    .expect("later allow wins");
}

// 15. JSONL persistence. DIVERGENCE: mhc keys the store by project (`.omo/permissions-approved.jsonl`), not agent dir.
#[test]
fn approved_rules_persist_across_service_reconstruction() {
    let dir = tempfile::tempdir().expect("project");
    let project = dir.path();
    assert!(load_approved(project).expect("load").is_empty());
    let rules = vec![
        rule("bash", "git *", Action::Allow),
        rule("edit", "src/*", Action::Allow),
    ];
    append_approved(project, &rules[..1]).expect("append");
    append_approved(project, &rules[1..]).expect("append");
    assert_eq!(load_approved(project).expect("load"), rules);
    clear_approved(project).expect("clear");
    assert!(load_approved(project).expect("load").is_empty());

    let mut h = harness(vec![rule("bash", "*", Action::Ask)], vec![]);
    let pending = h.service.ask(bash("req-persist", &["git commit"]));
    h.reply("req-persist", Reply::Always, None);
    settled(pending).expect("always");
    append_approved(project, &h.service.get_approved()).expect("append");
    let mut reopened = harness(
        vec![rule("bash", "*", Action::Ask)],
        load_approved(project).expect("load"),
    );
    settled(reopened.service.ask(bash("req-second", &["git push"]))).expect("persisted approval");
    assert_eq!(reopened.asked(), 0);
}

#[test]
fn approved_store_loads_even_when_project_settings_name_a_session_dir() {
    let dir = tempfile::tempdir().expect("project");
    let project = dir.path();
    let session_dir = tempfile::tempdir().expect("session dir");
    append_approved(project, &[rule("bash", "git *", Action::Allow)]).expect("append");
    std::fs::write(
        project.join(".omo/settings.json"),
        json!({"sessionDir": session_dir.path()}).to_string(),
    )
    .expect("settings");
    let text = project.to_string_lossy();
    let manager = maho_core::settings_manager::SettingsManager::create(
        &text,
        &project.join("agent").to_string_lossy(),
        &text,
        true,
    );
    let (_, approved) = maho_ext_permission_system::settings::load_permission_settings(
        &manager,
        &[],
        (project, &*text),
        None,
    )
    .expect("settings");
    assert_eq!(approved, [rule("bash", "git *", Action::Allow)]);
}

// additional: evaluate / merge / expand
#[test]
fn evaluate_merge_and_expand_contracts() {
    assert_eq!(
        evaluate(
            "unknown_tool",
            "anything",
            &[&[rule("bash", "*", Action::Allow)]]
        )
        .action,
        Action::Ask
    );
    assert_eq!(
        evaluate(
            "bash",
            "rm",
            &[&[
                rule("bash", "*", Action::Allow),
                rule("bash", "rm", Action::Deny)
            ]]
        )
        .action,
        Action::Deny
    );
    assert_eq!(
        evaluate(
            "edit",
            "src/components/Button.tsx",
            &[&[rule("edit", "src/*", Action::Allow)]]
        )
        .action,
        Action::Allow
    );
    assert_eq!(
        evaluate(
            "bash",
            "rm",
            &[
                &[rule("bash", "*", Action::Allow)],
                &[rule("bash", "rm", Action::Deny)]
            ]
        )
        .action,
        Action::Deny
    );
    let (a, b, c) = (
        [rule("read", "*", Action::Allow)],
        [rule("write", "*", Action::Ask)],
        [rule("bash", "*", Action::Deny)],
    );
    assert_eq!(
        merge(&[&a, &b, &c]),
        [a[0].clone(), b[0].clone(), c[0].clone()]
    );
    let ordered = [
        rule("edit", "src/*", Action::Allow),
        rule("edit", "src/secret/*", Action::Deny),
    ];
    let later = [rule("edit", "src/secret/ok.ts", Action::Allow)];
    assert_eq!(
        merge(&[&ordered, &later]),
        [ordered[0].clone(), ordered[1].clone(), later[0].clone()]
    );
    let home = "/home/tester";
    assert_eq!(expand("~", home), home);
    assert_eq!(expand("~/projects/foo", home), "/home/tester/projects/foo");
    assert_eq!(expand("$HOME", home), home);
    assert_eq!(expand("/some/~/path", home), "/some/~/path");
}

// additional: parser registry
#[test]
fn registry_parses_read_grep_find_ls_and_unknown() {
    let cwd = Path::new("/project");
    let unknown = parse("unknown_tool", json!({}), cwd);
    assert_eq!(
        (
            unknown.len(),
            unknown[0].permission.as_str(),
            unknown[0].patterns.clone()
        ),
        (1, "unknown_tool", vec!["*".to_owned()])
    );
    let read = parse("read", json!({"path":"src/index.ts"}), cwd);
    assert_eq!(
        (read[0].permission.as_str(), read[0].patterns.clone()),
        ("read", vec!["src/index.ts".to_owned()])
    );
    assert_eq!(
        parse("read", json!({"file_path":"README.md"}), cwd)[0].patterns,
        ["README.md"]
    );
    let grep = parse("grep", json!({"pattern":"foo","path":"src"}), cwd);
    assert_eq!(
        (grep[0].permission.as_str(), grep[0].patterns.clone()),
        ("grep", vec!["src".to_owned()])
    );
    let find = parse("find", json!({"path":"src"}), cwd);
    assert_eq!(
        (find[0].permission.as_str(), find[0].patterns.clone()),
        ("list", vec!["src".to_owned()])
    );
    let ls = parse("ls", json!({}), cwd);
    assert_eq!(
        (ls[0].permission.as_str(), ls[0].patterns.clone()),
        ("list", vec![".".to_owned()])
    );
}

// additional: non-interactive handler
#[test]
fn no_ui_handler_refuses_unmatched_requests() {
    let outcome = handle_no_ui(
        &bash("req-noui-3", &["ls", "pwd"]),
        false,
        &PermissionEventEmitter::default(),
    )
    .expect("emit");
    assert_eq!(
        serde_json::to_value(outcome).expect("json"),
        json!({"requestID":"req-noui-3","reply":"reject","message":format!("Permission required for bash (ls, pwd), and there is no UI to ask, so the call did not run. {DENIAL_IS_FINAL} The user can allow it with --permission bash=allow.")})
    );
}

// additional: complex scenarios
#[test]
fn generated_ids_unknown_replies_and_defensive_list() {
    let mut h = harness(vec![rule("bash", "*", Action::Ask)], vec![]);
    for (index, pattern) in [(1, "ls"), (2, "pwd")] {
        let pending = h
            .service
            .ask(request("", "session-1", "bash", &[pattern], &["*"]));
        let id = h.service.list()[0].id.clone();
        assert_eq!(id, format!("permission-{index}"));
        h.reply(&id, Reply::Once, None);
        settled(pending).expect("once");
    }
    let mut quiet = harness(vec![rule("bash", "*", Action::Allow)], vec![]);
    quiet.reply("unknown-id", Reply::Once, None);
    assert!(quiet.service.list().is_empty());
    assert!(quiet.replied().is_empty());

    let pending = h.service.ask(bash("req-copy", &["git commit"]));
    let mut copy = h.service.list();
    copy[0].patterns.push("modified".into());
    assert!(
        !h.service.list()[0]
            .patterns
            .contains(&"modified".to_owned())
    );
    h.reply("req-copy", Reply::Once, None);
    settled(pending).expect("once");
}
