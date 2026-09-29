use super::*;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;

fn home() -> String {
    std::env::var("HOME").expect("HOME")
}

fn temp_home_root(prefix: &str) -> (TempDir, String) {
    let dir = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(home())
        .expect("tempdir");
    let path = dir.path().to_string_lossy().into_owned();
    (dir, path)
}

fn user_path() -> String {
    join(&home(), &[".codex", "lsp-client.json"])
}

fn decisions_path() -> String {
    join(&home(), &[".codex", "lsp-install-decisions.json"])
}

fn context_value(cwd: &str, project: &[String]) -> Value {
    json!({
        "cwd": cwd,
        "projectConfigPaths": project,
        "userConfigPath": user_path(),
        "installDecisionsPath": decisions_path(),
        "capabilities": { "installDecisionTool": true },
    })
}

#[test]
fn exact_typed_context_canonicalizes_cwd_and_preserves_typed_paths() {
    let (_guard, root) = temp_home_root("lsp-context-root-");
    let project = join(&root, &[".codex", "lsp-client.json"]);
    std::fs::create_dir_all(join(&root, &[".codex"])).expect("mkdir");

    let context = parse_lsp_request_context(&json!({
        "cwd": join(&root, &["."]),
        "projectConfigPaths": [project],
        "userConfigPath": user_path(),
        "installDecisionsPath": decisions_path(),
        "capabilities": { "installDecisionTool": false },
    }))
    .expect("context");

    assert_eq!(
        context,
        LspRequestContext {
            cwd: root.clone(),
            project_config_paths: vec![project],
            user_config_path: user_path(),
            install_decisions_path: decisions_path(),
            capabilities: LspRequestCapabilities {
                install_decision_tool: false
            },
        }
    );
    assert_eq!(
        run_with_request_context(context.clone(), lsp_request_context),
        Ok(context)
    );
}

#[test]
fn no_active_context_returns_typed_unavailable_error() {
    assert_eq!(
        lsp_request_context(),
        Err(LspRequestContextUnavailableError)
    );
}

#[test]
fn unknown_field_rejects_before_lookup() {
    let (_guard, root) = temp_home_root("lsp-context-unknown-");
    let mut value = context_value(&root, &[join(&root, &[".codex", "lsp-client.json"])]);
    value["env"] = json!({});

    assert!(parse_lsp_request_context(&value).is_err());
}

#[test]
fn project_path_outside_cwd_rejects_before_config_loading() {
    let (_guard, root) = temp_home_root("lsp-context-confined-");
    let (_outside_guard, outside) = temp_home_root("lsp-context-outside-");

    let error = parse_lsp_request_context(&context_value(
        &root,
        &[join(&outside, &["lsp-client.json"])],
    ))
    .expect_err("outside");

    assert_eq!(error.code, "project_config_outside_cwd");
}

#[test]
fn equivalent_macos_var_spelling_canonicalizes_to_one_identity() {
    let dir = tempfile::Builder::new()
        .prefix("lsp-context-private-var-")
        .tempdir()
        .expect("tempdir");
    let system_root = dir.path().to_string_lossy().into_owned();
    let canonical_root = std::fs::canonicalize(&system_root)
        .expect("canonical")
        .to_string_lossy()
        .into_owned();
    let alias_root = match canonical_root.strip_prefix("/private/var/") {
        Some(rest) => format!("/var/{rest}"),
        None => system_root,
    };
    std::fs::create_dir_all(join(&canonical_root, &[".codex"])).expect("mkdir");

    let context = parse_lsp_request_context(&context_value(
        &alias_root,
        &[join(&alias_root, &[".codex", "lsp-client.json"])],
    ))
    .expect("context");

    assert_eq!(
        (context.cwd, context.project_config_paths),
        (
            canonical_root.clone(),
            vec![join(&canonical_root, &[".codex", "lsp-client.json"])]
        )
    );
}

#[test]
fn missing_standard_project_config_inside_cwd_is_accepted_with_intended_suffix() {
    let (_guard, root) = temp_home_root("lsp-context-missing-config-");
    std::fs::create_dir_all(join(&root, &[".codex"])).expect("mkdir");
    let missing = join(&root, &[".codex", "missing-lsp-client.json"]);

    let context = parse_lsp_request_context(&context_value(&root, std::slice::from_ref(&missing)))
        .expect("context");

    assert_eq!(context.project_config_paths, vec![missing]);
}

#[cfg(unix)]
#[test]
fn symlink_project_config_escaping_cwd_is_rejected() {
    let (_guard, root) = temp_home_root("lsp-context-symlink-root-");
    let (_outside_guard, outside) = temp_home_root("lsp-context-symlink-outside-");
    let outside_config = join(&outside, &["lsp-client.json"]);
    std::fs::write(&outside_config, "{}").expect("write");
    let linked = join(&root, &["linked-lsp-client.json"]);
    std::os::unix::fs::symlink(&outside_config, &linked).expect("symlink");

    let error = parse_lsp_request_context(&context_value(&root, &[linked])).expect_err("escape");

    assert_eq!(error.code, "project_config_outside_cwd");
}

#[test]
fn standalone_mcp_env_resolves_exact_defaults_and_relative_paths() {
    let (_guard, root) = temp_home_root("lsp-context-standalone-");
    let (_home_guard, home_dir) = temp_home_root("lsp-context-home-");
    let absolute_project = join(&root, &[".omo", "lsp-client.json"]);
    let env = HashMap::from([
        (
            "LSP_TOOLS_MCP_PROJECT_CONFIG".to_string(),
            ["relative.json", "", absolute_project.as_str()].join(DELIMITER),
        ),
        (
            "LSP_TOOLS_MCP_USER_CONFIG".to_string(),
            "user-lsp.json".to_string(),
        ),
        (
            "LSP_TOOLS_MCP_INSTALL_DECISIONS".to_string(),
            "decisions.json".to_string(),
        ),
    ]);

    let context = create_standalone_mcp_request_context(StandaloneMcpRequestContextInput {
        cwd: Some(root.clone()),
        env: Some(env),
        home_dir: Some(home_dir.clone()),
    })
    .expect("context");

    assert_eq!(
        context,
        LspRequestContext {
            cwd: root.clone(),
            project_config_paths: vec![join(&root, &["relative.json"]), absolute_project],
            user_config_path: join(&home_dir, &["user-lsp.json"]),
            install_decisions_path: join(&home_dir, &["decisions.json"]),
            capabilities: LspRequestCapabilities {
                install_decision_tool: true
            },
        }
    );
}
