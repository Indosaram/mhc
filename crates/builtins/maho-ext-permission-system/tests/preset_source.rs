use maho_ext_permission_system::{config::{merge, rules_for_preset}, evaluate::evaluate, events::PermissionEventEmitter, non_interactive::handle_no_ui, parsers::create_builtin_parser_registry, types::*};
use serde_json::json;
use std::{path::Path, sync::{Arc, Mutex}};

#[test]
fn later_presets_restore_ask_boundaries_after_full_access() {
    for (preset, cases) in [
        (PermissionPresetName::Workspace, vec![("bash", "git status", Action::Allow), ("external_directory", "../outside", Action::Ask), ("unknown_tool", "*", Action::Ask)]),
        (PermissionPresetName::ReadOnly, vec![("read", "README.md", Action::Allow), ("bash", "git status", Action::Ask), ("edit", "src/index.ts", Action::Ask), ("unknown_tool", "*", Action::Ask)]),
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
fn no_ui_preset_admission_emits_only_source_events() {
    for (preset, allowed) in [(PermissionPresetName::FullAccess, true), (PermissionPresetName::ReadOnly, false)] {
        let emitter = PermissionEventEmitter::default();
        let events = Arc::new(Mutex::new(Vec::new()));
        let captured = events.clone();
        let _asked = emitter.on_asked(move |_| captured.lock().expect("events").push("asked"));
        let captured = events.clone();
        let _replied = emitter.on_replied(move |event| { assert_eq!(event.reply, PermissionDecision::Allow); captured.lock().expect("events").push("replied"); });
        let request = Request { id:"request-1".into(), session_id:"session-1".into(), permission:"bash".into(), patterns:vec!["git status".into()], always:vec!["*".into()], metadata:Default::default(), tool:None };
        let result = handle_no_ui(&request, (&rules_for_preset(preset), &[]), &emitter).expect("admission");
        assert_eq!(result.is_none(), allowed);
        if let Some(reply) = result { assert_eq!(reply.request_id, "request-1"); assert_eq!(reply.reply, Reply::Reject); }
        assert_eq!(*events.lock().expect("events"), if allowed {vec!["asked", "replied"]} else {vec!["asked"]});
    }
}

#[test]
fn workspace_direct_tools_require_external_admission() {
    let cwd = Path::new("/Users/me/project");
    let registry = create_builtin_parser_registry();
    let rules = rules_for_preset(PermissionPresetName::Workspace);
    for (tool, input) in [("read", json!({"path":"../secret.txt"})), ("write",json!({"path":"/tmp/outside.txt","content":"hello"})), ("grep",json!({"pattern":"token","path":"/tmp"})), ("ls",json!({"path":"/tmp","limit":20}))] {
        let requests = registry.parse(tool, &input, (cwd, cwd));
        let external = requests.iter().find(|request|request.permission=="external_directory").expect("external request");
        assert_eq!(evaluate("external_directory", &external.patterns[0], &[&rules]).action, Action::Ask, "{tool}");
    }
}

#[test]
fn root_level_external_approval_does_not_broaden_to_root() {
    let cwd = Path::new("/Users/me/project");
    let registry = create_builtin_parser_registry();
    for (tool, path, always) in [("ls","/tmp","/tmp/*"),("read","/tmp/file.txt","/tmp/*"),("read","/secret.txt","/secret.txt"),("read","/secret","/secret")] {
        let requests = registry.parse(tool, &json!({"path":path}), (cwd, cwd));
        let external = requests.iter().find(|request|request.permission=="external_directory").expect("external request");
        assert_eq!(external.always, [always], "{tool} {path}");
    }
}
