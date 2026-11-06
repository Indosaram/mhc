use maho_ext_permission_system::parsers::create_builtin_parser_registry;
use serde_json::json;
use std::path::Path;
fn parse(
    name: &str,
    input: serde_json::Value,
) -> Vec<maho_ext_permission_system::parsers::PermissionRequest> {
    create_builtin_parser_registry().parse(
        name,
        &input,
        (Path::new("/workspace/project"), Path::new("/home/user")),
    )
}
#[test]
fn bash_prefix() {
    let r = parse("bash", json!({"command":"git status"}));
    assert_eq!(r[0].permission, "bash");
    assert_eq!(r[0].patterns, vec!["git status"]);
}
#[test]
fn bash_timeout() {
    assert_eq!(
        parse("bash", json!({"command":"sleep 5","timeout":10}))[0].permission,
        "bash"
    );
}
#[test]
fn stdin_class() {
    assert_eq!(
        parse("bash_input", json!({"input":"rm -rf /etc"}))[0].permission,
        "bash"
    );
}
#[test]
fn file_tools() {
    for name in ["edit", "write", "multiedit", "read"] {
        let r = parse(name, json!({"path":"src/index.ts"}));
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].patterns, vec!["src/index.ts"]);
        assert_eq!(
            r[0].permission,
            if name == "read" { "read" } else { "edit" }
        );
    }
}
#[test]
fn aliases() {
    assert_eq!(
        parse("read", json!({"file_path":"src/foo"}))[0].patterns,
        vec!["src/foo"]
    );
}
#[test]
fn read_permission_alias_scopes_external_always_to_parent() {
    let requests = parse("read_permission", json!({"path":"/mhc-outside/config"}));
    assert_eq!(requests[0].permission, "read");
    assert_eq!(requests[1].permission, "external_directory");
    assert_eq!(requests[1].always, vec!["/mhc-outside/*"]);
}
#[test]
fn external_file() {
    let r = parse("write", json!({"path":"/mhc-outside/config"}));
    assert_eq!(r[1].permission, "external_directory");
    assert_eq!(r[1].always, vec!["/mhc-outside/*"]);
}
#[test]
fn grep_pattern() {
    assert_eq!(
        parse("grep", json!({"pattern":"TODO"}))[0].patterns,
        vec!["TODO"]
    );
}
#[test]
fn grep_path() {
    let r = parse("grep", json!({"path":"/mhc-outside","pattern":"TODO"}));
    assert_eq!(r[0].patterns, vec!["/mhc-outside"]);
    assert_eq!(r[1].always, vec!["/mhc-outside/*"]);
}
#[test]
fn grep_empty_path_does_not_fall_back_to_pattern() {
    // Given an explicitly empty path and a nonempty pattern.
    let input = json!({"path":"","pattern":"TODO"});
    // When the grep permission is parsed.
    let requests = parse("grep", input);
    // Then the string path is the one search path (senpi v2026.10.27-2 array-aware grep), not the pattern.
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].permission, "grep");
    assert_eq!(requests[0].patterns, vec![""]);
}
#[test]
fn grep_path_array_gates_every_element() {
    // Given a grep over a workspace path and an external one (senpi#4311).
    let requests = parse(
        "grep",
        json!({"pattern":"TOKEN","path":["src","/mhc-outside"]}),
    );
    // Then each element is a grep pattern and only the external one is classified.
    assert_eq!(requests[0].patterns, vec!["src", "/mhc-outside"]);
    assert_eq!(requests[1].permission, "external_directory");
    assert_eq!(requests[1].patterns, vec!["/mhc-outside"]);
    assert_eq!(requests[1].always, vec!["/mhc-outside/*"]);
}
#[test]
fn custom_parser_empty_yields_zero_requests() {
    let mut registry = create_builtin_parser_registry();
    registry.set_definition_resolver(std::sync::Arc::new(|name| {
        if name == "omo_browser_bridge" {
            let mut def = maho_ext_api::ToolDefinition::new(
                "omo_browser_bridge",
                "desc",
                json!({}),
                std::sync::Arc::new(|_| Box::pin(async { panic!() })),
            );
            def.permission_parser = Some(std::sync::Arc::new(|_input, _cwd| Vec::new()));
            Some(def)
        } else {
            None
        }
    }));
    let requests = registry.parse(
        "omo_browser_bridge",
        &json!({}),
        (Path::new("/workspace"), Path::new("/home/user")),
    );
    assert!(requests.is_empty());
}
#[test]
fn custom_parser_returns_custom_requests() {
    let mut registry = create_builtin_parser_registry();
    registry.set_definition_resolver(std::sync::Arc::new(|name| {
        if name == "custom_tool" {
            let mut def = maho_ext_api::ToolDefinition::new(
                "custom_tool",
                "desc",
                json!({}),
                std::sync::Arc::new(|_| Box::pin(async { panic!() })),
            );
            def.permission_parser = Some(std::sync::Arc::new(|input, _cwd| {
                let target = input
                    .get("target")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("default");
                vec![maho_ext_api::ToolPermissionRequest::new(
                    "custom_perm",
                    vec![target.to_string()],
                    vec![format!("{target}/*")],
                )]
            }));
            Some(def)
        } else {
            None
        }
    }));
    let requests = registry.parse(
        "custom_tool",
        &json!({"target": "resource"}),
        (Path::new("/workspace"), Path::new("/home/user")),
    );
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].permission, "custom_perm");
    assert_eq!(requests[0].patterns, vec!["resource"]);
    assert_eq!(requests[0].always, vec!["resource/*"]);
}
#[test]
fn list_default() {
    for name in ["find", "ls"] {
        let r = parse(name, json!({}));
        assert_eq!(r[0].permission, "list");
        assert_eq!(r[0].patterns, vec!["."]);
    }
}
#[test]
fn glob_uses_list_permission() {
    let r = parse("glob", json!({"path":"src"}));
    assert_eq!(r[0].permission, "list");
    assert_eq!(r[0].patterns, vec!["src"]);
}
#[test]
fn fallback() {
    assert_eq!(parse("unknown", json!({}))[0].patterns, vec!["*"]);
}
#[test]
fn apply_patch_explicit_file_uses_edit_permission() {
    let requests = parse("apply_patch", json!({"path":"src/file.rs"}));
    assert_eq!(requests[0].permission, "edit");
    assert_eq!(requests[0].patterns, vec!["src/file.rs"]);
}

/// The read tool unquotes a path that does not exist as written, so the parser must classify the
/// unquoted file: a quoted outside path still asks external_directory, and a quoted credential
/// file still asks credential_file.
#[test]
fn quoted_read_path_is_classified_as_the_file_the_read_tool_opens() {
    let project = tempfile::tempdir().expect("project");
    let outside = tempfile::tempdir().expect("outside");
    let secret = outside.path().join("secret.txt");
    std::fs::write(&secret, "x").expect("secret");
    let secret = std::fs::canonicalize(&secret).expect("canonical secret");
    let home = Path::new("/home/user");
    for quoted in [
        format!("'{}'", secret.display()),
        format!("\"{}\"", secret.display()),
        format!("@'{}'", secret.display()),
    ] {
        let requests = create_builtin_parser_registry().parse(
            "read",
            &json!({ "path": quoted }),
            (project.path(), home),
        );
        let external = requests
            .iter()
            .find(|request| request.permission == "external_directory")
            .unwrap_or_else(|| panic!("{quoted}: external_directory request in {requests:?}"));
        assert_eq!(
            external.patterns,
            vec![secret.to_string_lossy().into_owned()]
        );
        assert_eq!(external.original_paths, Some(vec![quoted.clone()]));
        assert_eq!(
            maho_ext_permission_system::parsers::resolve_read_path(&quoted, project.path()),
            secret.to_string_lossy()
        );
    }

    let agent = tempfile::tempdir().expect("agent");
    std::fs::write(agent.path().join("auth.json"), "{}").expect("auth");
    let registry =
        maho_ext_permission_system::parsers::create_builtin_parser_registry_in(agent.path());
    let quoted = format!("'{}'", agent.path().join("auth.json").display());
    let requests = registry.parse("read", &json!({ "path": quoted }), (project.path(), home));
    assert!(
        requests
            .iter()
            .any(|request| request.permission == "credential_file"),
        "{requests:?}"
    );
}

#[test]
fn a_quoted_path_that_exists_as_written_is_not_unquoted() {
    let project = tempfile::tempdir().expect("project");
    let literal = project.path().join("'name'");
    std::fs::write(&literal, "x").expect("literal");
    assert_eq!(
        maho_ext_permission_system::parsers::resolve_read_path("'name'", project.path()),
        literal.to_string_lossy()
    );
    // Neither the quoted nor the unquoted spelling exists: the unquoted path is returned.
    assert_eq!(
        maho_ext_permission_system::parsers::resolve_read_path("'missing'", project.path()),
        project.path().join("missing").to_string_lossy()
    );
}
