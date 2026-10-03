use maho_ext_permission_system::parsers::create_builtin_parser_registry;
use serde_json::json;

#[test]
fn patch_paths_gate_edits_and_external_directories() {
    // Given a patch touching a workspace file and an external move destination.
    let project = tempfile::tempdir().expect("project");
    let input = json!({"input":"*** Begin Patch\n*** Update File: src/a.rs\n*** Move to: /outside/b.rs\n*** End Patch"});
    // When the registered builtin parser processes it.
    let requests = create_builtin_parser_registry().parse("apply_patch", &input, (project.path(), project.path()));
    // Then every patched path is gated, including external access.
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0].patterns, ["src/a.rs"]);
    assert_eq!(requests[1].patterns, ["/outside/b.rs"]);
    assert_eq!(requests[2].permission, "external_directory");
}

#[test]
fn path_monitor_requires_read_and_external_directory() {
    // Given a path-mode monitor outside the workspace.
    let project = tempfile::tempdir().expect("project");
    let input = json!({"path":"/outside/log"});
    // When permission requests are extracted.
    let requests = create_builtin_parser_registry().parse("monitor", &input, (project.path(), project.path()));
    // Then the monitor cannot bypass read and external-directory policy.
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].permission, "read");
    assert_eq!(requests[0].patterns, ["/outside/log"]);
    assert_eq!(requests[1].permission, "external_directory");
}

#[test]
fn command_monitor_uses_bash_admission() {
    // Given a command-mode monitor.
    let project = tempfile::tempdir().expect("project");
    let input = json!({"command":"git status"});
    // When permission requests are extracted.
    let requests = create_builtin_parser_registry().parse("monitor", &input, (project.path(), project.path()));
    // Then command monitoring shares bash permissions.
    assert_eq!(requests[0].permission, "bash");
    assert_eq!(requests[0].patterns, ["git status"]);
}

#[test]
fn patch_aliases_preserve_per_file_scope_and_empty_fallback() {
    let project = tempfile::tempdir().expect("project");
    let registry = create_builtin_parser_registry();
    for tool in ["edit", "write", "apply_patch", "multiedit"] {
        for key in ["input", "patchText"] {
            let input = json!({(key):"*** Begin Patch\n*** Add File: a.txt\n*** End Patch"});
            let requests = registry.parse(tool, &input, (project.path(), project.path()));
            assert_eq!(requests[0].permission, "edit");
            assert_eq!(requests[0].patterns, ["a.txt"]);
            assert_eq!(requests[0].always, ["a.txt"]);
        }
        for input in [json!({}), json!({"input":""}), json!({"input":"not a patch"})] {
            let requests = registry.parse(tool, &input, (project.path(), project.path()));
            assert_eq!(requests[0].permission, "edit");
            assert_eq!(requests[0].patterns, ["*"]);
        }
    }
    assert_eq!(registry.parse("monitor", &json!({}), (project.path(), project.path()))[0].permission, "bash");
}
