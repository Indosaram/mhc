use maho_ext_permission_system::{
    config::{merge, rules_for_preset},
    evaluate::evaluate,
    events::PermissionEventEmitter,
    non_interactive::handle_no_ui,
    parsers::create_builtin_parser_registry,
    types::*,
};
use serde_json::json;
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

#[test]
fn later_presets_restore_ask_boundaries_after_full_access() {
    for (preset, cases) in [
        (
            PermissionPresetName::Workspace,
            vec![
                ("bash", "git status", Action::Allow),
                ("external_directory", "../outside", Action::Ask),
                ("unknown_tool", "*", Action::Ask),
            ],
        ),
        (
            PermissionPresetName::AcceptEdits,
            vec![
                ("edit", "src/index.ts", Action::Allow),
                ("bash", "git status", Action::Ask),
                ("external_directory", "../outside", Action::Ask),
            ],
        ),
        (
            PermissionPresetName::Auto,
            vec![
                ("read", "README.md", Action::Ask),
                ("bash", "git status", Action::Ask),
            ],
        ),
        (
            PermissionPresetName::ReadOnly,
            vec![
                ("read", "README.md", Action::Allow),
                ("bash", "git status", Action::Ask),
                ("edit", "src/index.ts", Action::Ask),
                ("unknown_tool", "*", Action::Ask),
            ],
        ),
        (
            PermissionPresetName::Auto,
            vec![
                ("read", "README.md", Action::Ask),
                ("bash", "git status", Action::Ask),
                ("edit", "src/index.ts", Action::Ask),
            ],
        ),
    ] {
        let earlier = rules_for_preset(PermissionPresetName::FullAccess);
        let later = rules_for_preset(preset);
        let rules = merge(&[&earlier, &later]);
        for (permission, pattern, expected) in cases {
            assert_eq!(evaluate(permission, pattern, &[&rules]).action, expected);
        }
    }
}

#[test]
fn no_ui_requests_are_rejected_when_a_preset_still_requires_confirmation() {
    let emitter = PermissionEventEmitter::default();
    let events = Arc::new(Mutex::new(Vec::new()));
    let captured = events.clone();
    let _asked = emitter.on_asked(move |_| captured.lock().expect("events").push("asked"));
    let captured = events.clone();
    let _replied = emitter.on_replied(move |_| captured.lock().expect("events").push("replied"));
    let request = Request {
        id: "request-1".into(),
        session_id: "session-1".into(),
        permission: "bash".into(),
        patterns: vec!["git status".into()],
        always: vec!["*".into()],
        original_paths: None,
        metadata: Default::default(),
        wider_always: None,
        tool: None,
    };
    let reply = handle_no_ui(&request, false, &emitter).expect("admission");
    assert_eq!(reply.request_id, "request-1");
    assert_eq!(reply.reply, Reply::Reject);
    assert_eq!(reply.message.as_deref(), Some(format!("Permission required for bash (git status), and there is no UI to ask, so the call did not run. {DENIAL_IS_FINAL} The user can allow it with --permission bash=allow.").as_str()));
    assert_eq!(*events.lock().expect("events"), vec!["asked"]);
}

#[test]
fn workspace_direct_tools_require_external_admission() {
    let cwd = Path::new("/Users/me/project");
    let registry = create_builtin_parser_registry();
    let rules = rules_for_preset(PermissionPresetName::Workspace);
    for (tool, input) in [
        ("read", json!({"path":"../secret.txt"})),
        (
            "write",
            json!({"path":"/tmp/outside.txt","content":"hello"}),
        ),
        ("grep", json!({"pattern":"token","path":"/tmp"})),
        ("ls", json!({"path":"/tmp","limit":20})),
    ] {
        let requests = registry.parse(tool, &input, (cwd, cwd));
        let external = requests
            .iter()
            .find(|request| request.permission == "external_directory")
            .expect("external request");
        assert_eq!(
            evaluate("external_directory", &external.patterns[0], &[&rules]).action,
            Action::Ask,
            "{tool}"
        );
    }
}

#[test]
fn root_level_external_approval_does_not_broaden_to_root() {
    let cwd = Path::new("/Users/me/project");
    let registry = create_builtin_parser_registry();
    // senpi c028c0a4e4: targets are canonical, so start from the real temp directory and its root.
    let temp = std::fs::canonicalize(std::env::temp_dir())
        .expect("real temp")
        .to_string_lossy()
        .into_owned();
    let temp = temp
        .strip_prefix(r"\\?\")
        .map_or(temp.clone(), str::to_owned);
    let root = Path::new(&temp)
        .ancestors()
        .last()
        .expect("root")
        .to_string_lossy()
        .into_owned();
    let join =
        |parent: &str, child: &str| Path::new(parent).join(child).to_string_lossy().into_owned();
    let (file, secret_txt, secret) = (
        join(&temp, "file.txt"),
        join(&root, "secret.txt"),
        join(&root, "secret"),
    );
    let temp_always = format!("{temp}/*");
    for (tool, path, always) in [
        ("ls", temp.as_str(), temp_always.as_str()),
        ("read", file.as_str(), temp_always.as_str()),
        ("read", secret_txt.as_str(), secret_txt.as_str()),
        ("read", secret.as_str(), secret.as_str()),
    ] {
        let requests = registry.parse(tool, &json!({"path":path}), (cwd, cwd));
        let external = requests
            .iter()
            .find(|request| request.permission == "external_directory")
            .expect("external request");
        assert_eq!(external.always, [always], "{tool} {path}");
    }
}
