//! Port of senpi test/mcp/3858-reference-trust.test.ts: which sources may declare configuration references.
//!
//! Upstream spies on `fs.open` to prove a refused reference is never read. Here every `$file` names a path that
//! does not exist: a read would fail as `missing`, so a `forbidden` refusal proves no reference I/O happened.

use maho_core::config_reference_resolver::ConfigValue;
use maho_ext_mcp::{
    config::{load_mcp_config, resolve_extension_mcp_server},
    config_references::extract_mcp_references,
    config_schema::{LoadMcpConfigOptions, McpServerSource, McpServerState, ServerConfigWire},
    host_registry::HostMcpRegistry,
    service::McpService,
    skills::{SkillLike, parse_skill_mcp_declarations},
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path, sync::Arc};

fn unread(root: &Path) -> String {
    root.join("unread").to_string_lossy().into_owned()
}

fn wire(mut raw: Value) -> ServerConfigWire {
    let references = extract_mcp_references(&mut raw);
    let mut server: ServerConfigWire = serde_json::from_value(raw).expect("plain declaration");
    server.references = references;
    server
}

/// `it.each(["remote-skill", "untrusted-skill"])`: a skill server that may not declare references is skipped
/// with a warning naming the forbidden reference, and is never added to the session config.
async fn refused_skill_warning(remote: bool) {
    let root = tempfile::tempdir().expect("skill root");
    let agent = tempfile::tempdir().expect("agent dir");
    let mut server = json!({
        "command": "fixture",
        "headers": {"Authorization": {"$file": unread(root.path())}},
    });
    if remote {
        server["type"] = json!("http");
        server["url"] = json!("https://example.invalid");
    }
    std::fs::write(
        root.path().join("mcp.json"),
        json!({ "fixture": server }).to_string(),
    )
    .expect("skill sidecar");
    let declarations = parse_skill_mcp_declarations(&[SkillLike {
        name: "fixture".into(),
        file_path: root.path().join("SKILL.md"),
        base_dir: root.path().into(),
        // A remote server refuses even from a trusted user skill; a project skill is untrusted.
        scope: (!remote).then(|| "project".to_owned()),
        ..Default::default()
    }]);
    assert!(declarations.servers.contains_key("fixture"));
    let registry = Arc::new(HostMcpRegistry::default());
    let mut service = McpService::new(registry.clone(), 3858);
    service
        .attach_session(root.path(), agent.path(), &BTreeMap::new(), false, &[])
        .await
        .expect("empty attach");
    let warnings = service
        .attach_skill_mcp_servers(&declarations)
        .await
        .expect("skill attach");
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(
        warnings[0].contains("Skill 'fixture' MCP server 'fixture' skipped"),
        "{}",
        warnings[0]
    );
    assert!(
        warnings[0].contains(
            "mcpServers.fixture.headers.Authorization: configuration reference forbidden"
        ),
        "{}",
        warnings[0]
    );
    assert!(
        service
            .config
            .as_ref()
            .is_none_or(|config| !config.servers.contains_key("fixture")),
        "a refused skill server must not be declared"
    );
    assert!(!service.connections.contains_key("fixture"));
    service.dispose().await.expect("dispose");
    registry.dispose().await.expect("registry dispose");
}

#[tokio::test]
async fn refuses_remote_skill_objects_without_reference_io() {
    refused_skill_warning(true).await;
}

#[tokio::test]
async fn refuses_untrusted_skill_objects_without_reference_io() {
    refused_skill_warning(false).await;
}

#[test]
fn refuses_extension_objects_without_reference_io() {
    let root = tempfile::tempdir().expect("root");
    let declaration = wire(json!({
        "command": "fixture",
        "headers": {"Authorization": {"$file": unread(root.path())}},
    }));
    let error = resolve_extension_mcp_server(
        "fixture",
        declaration,
        Path::new("extension.ts"),
        root.path(),
        Some(root.path()),
    )
    .expect_err("an extension may not declare a reference");
    assert_eq!(
        error.0,
        "mcpServers.fixture.headers.Authorization: configuration reference forbidden"
    );
}

/// The project's `.mcp.json` is the only Claude file: home and the Claude config dir are a separate empty dir,
/// so its trust follows `project_trusted` alone.
fn claude_options<'a>(
    root: &'a Path,
    home: &'a Path,
    env: &'a BTreeMap<String, String>,
    project_trusted: bool,
) -> LoadMcpConfigOptions<'a> {
    LoadMcpConfigOptions {
        cwd: root,
        agent_dir: root,
        env,
        project_trusted,
        home_dir: Some(home),
        claude_config_dir: Some(home),
    }
}

#[test]
fn keeps_trusted_claude_references_raw_until_the_connection_generation() {
    let root = tempfile::tempdir().expect("root");
    let reference = json!({"$file": unread(root.path())});
    std::fs::write(
        root.path().join("mcp.json"),
        json!({"settings": {"importConfigs": ["claude"]}}).to_string(),
    )
    .expect("global config");
    std::fs::write(
        root.path().join(".mcp.json"),
        json!({"mcpServers": {"fixture": {"command": "fixture", "args": [reference]}}}).to_string(),
    )
    .expect("claude config");
    let env = BTreeMap::new();
    let home = tempfile::tempdir().expect("home");
    let config = load_mcp_config(claude_options(root.path(), home.path(), &env, true))
        .expect("loads without reading");
    let server = &config.servers["fixture"];
    assert_eq!(server.source, McpServerSource::Claude);
    assert_eq!(server.state, McpServerState::Enabled);
    let declared = server.config.as_ref().expect("declared config");
    let Some(ConfigValue::Reference { origin, .. }) = declared.references.get("args.0") else {
        panic!(
            "the reference stays a raw declaration: {:?}",
            declared.references
        );
    };
    let origin = origin
        .as_ref()
        .expect("a trusted file remembers its origin");
    assert_eq!(origin.authority, "trusted");
    assert_eq!(origin.field, "mcpServers.fixture.args.0");
    assert_eq!(Path::new(&origin.origin), root.path().join(".mcp.json"));
    assert_eq!(
        declared.declared_json().expect("declared json")["args"][0],
        reference
    );
}

#[test]
fn keeps_untrusted_claude_declarations_as_name_only_placeholders() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(
        root.path().join("mcp.json"),
        json!({"settings": {"importConfigs": ["claude"]}}).to_string(),
    )
    .expect("global config");
    std::fs::write(
        root.path().join(".mcp.json"),
        json!({"mcpServers": {"fixture": {"command": "fixture", "args": [{"$file": "unread"}]}}})
            .to_string(),
    )
    .expect("claude config");
    let env = BTreeMap::new();
    let home = tempfile::tempdir().expect("home");
    let config =
        load_mcp_config(claude_options(root.path(), home.path(), &env, false)).expect("loads");
    let server = &config.servers["fixture"];
    assert_eq!(server.state, McpServerState::Untrusted);
    assert!(server.config.is_none());
    assert!(server.config_hash.is_none());
}
