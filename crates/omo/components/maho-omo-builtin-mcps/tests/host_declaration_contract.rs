mod support;

use std::collections::BTreeMap;
use std::path::Path;

use maho_ext_api::{Extension, McpExposure, McpLifecycle, McpServerDeclaration, McpTransport, RegisteredMcpServerDeclaration};
use maho_ext_mcp::config::{load_mcp_config, merge_extension_mcp_servers, validate_mcp_server_declaration};
use maho_ext_mcp::config_schema::{LoadMcpConfigOptions, McpServerSource, McpServerState, ResolvedMcpConfig, ServerConfigWire};
use maho_omo_builtin_mcps::{BuiltinMcpsComponent, CONTEXT7_API_KEY_ENV, CONTEXT7_SERVER_NAME, GREP_APP_SERVER_NAME};

fn declarations(api_key: Option<&str>) -> Vec<RegisteredMcpServerDeclaration> {
    let root = tempfile::tempdir().expect("temp");
    let env: BTreeMap<String, String> = api_key
        .map(|value| BTreeMap::from([(CONTEXT7_API_KEY_ENV.to_owned(), value.to_owned())]))
        .unwrap_or_default();
    let mut api = support::api(root.path());
    BuiltinMcpsComponent::from_env(&env).register(&mut api);
    api.registered.mcp_servers
}

fn load(cwd: &Path, agent_dir: &Path) -> ResolvedMcpConfig {
    load_mcp_config(LoadMcpConfigOptions {
        cwd,
        agent_dir,
        env: &BTreeMap::new(),
        project_trusted: false,
    })
    .expect("load config")
}

#[test]
fn given_the_anonymous_and_bearer_declarations_when_the_installed_host_validator_runs_then_every_declaration_is_accepted() {
    for api_key in [None, Some("ctx7sk-live-secret")] {
        let declared = declarations(api_key);
        assert_eq!(declared.iter().map(|entry| entry.name.as_str()).collect::<Vec<_>>(), [CONTEXT7_SERVER_NAME, GREP_APP_SERVER_NAME]);

        let errors: Vec<Option<String>> = declared
            .iter()
            .map(|entry| validate_mcp_server_declaration(&entry.name, serde_json::to_value(ServerConfigWire::from(&entry.config)).expect("wire json")))
            .collect();
        assert_eq!(errors, [None, None], "{api_key:?}");
    }
}

#[test]
fn given_no_mcp_json_when_the_declarations_merge_then_both_servers_are_present_as_enabled_extensions() {
    let root = tempfile::tempdir().expect("temp");
    let cwd = root.path().join("project");
    let agent_dir = root.path().join("agent");
    std::fs::create_dir_all(&cwd).expect("cwd");
    std::fs::create_dir_all(&agent_dir).expect("agent dir");

    let mut config = load(&cwd, &agent_dir);
    merge_extension_mcp_servers(&mut config, &declarations(None)).expect("merge");

    assert_eq!(config.servers.keys().cloned().collect::<Vec<_>>(), [CONTEXT7_SERVER_NAME, GREP_APP_SERVER_NAME]);
    for name in [CONTEXT7_SERVER_NAME, GREP_APP_SERVER_NAME] {
        let server = &config.servers[name];
        assert_eq!(server.source, McpServerSource::Extension);
        assert_eq!(server.state, McpServerState::Enabled);
        let resolved = server.config.as_ref().expect("resolved config");
        assert_eq!(resolved.transport, Some(McpTransport::Http));
        assert_eq!(resolved.lifecycle, Some(McpLifecycle::Lazy));
        assert_eq!(resolved.exposure, Some(McpExposure::Search));
    }
    assert!(config.diagnostics.is_empty(), "{:?}", config.diagnostics);
}

#[test]
fn given_a_trusted_global_mcp_json_entry_of_the_same_name_when_the_declarations_merge_then_the_global_config_wins() {
    let root = tempfile::tempdir().expect("temp");
    let cwd = root.path().join("project");
    let agent_dir = root.path().join("agent");
    std::fs::create_dir_all(&cwd).expect("cwd");
    std::fs::create_dir_all(&agent_dir).expect("agent dir");
    std::fs::write(agent_dir.join("mcp.json"), r#"{"mcpServers":{"context7":{"enabled":false}}}"#).expect("global mcp.json");

    let mut config = load(&cwd, &agent_dir);
    assert_eq!(config.servers[CONTEXT7_SERVER_NAME].source, McpServerSource::Global);
    merge_extension_mcp_servers(&mut config, &declarations(Some("ctx7sk-live-secret"))).expect("merge");

    let context7 = &config.servers[CONTEXT7_SERVER_NAME];
    assert_eq!(context7.source, McpServerSource::Global);
    assert_eq!(context7.state, McpServerState::Disabled);
    assert_eq!(context7.config.as_ref().and_then(|server| server.bearer_token_env.clone()), None);

    assert_eq!(config.servers[GREP_APP_SERVER_NAME].source, McpServerSource::Extension);
    assert_eq!(config.servers[GREP_APP_SERVER_NAME].state, McpServerState::Enabled);

    let diagnostics = config.diagnostics.join("\n");
    assert!(diagnostics.contains("Extension MCP server 'context7'"), "{diagnostics}");
    assert!(diagnostics.contains("global config at"), "{diagnostics}");
    assert!(diagnostics.contains("wins."), "{diagnostics}");
}

#[test]
fn given_an_untrusted_project_placeholder_of_the_same_name_when_the_declarations_merge_then_the_extension_declaration_replaces_it() {
    let root = tempfile::tempdir().expect("temp");
    let cwd = root.path().join("project");
    let agent_dir = root.path().join("agent");
    std::fs::create_dir_all(cwd.join(".maho")).expect("project config dir");
    std::fs::create_dir_all(&agent_dir).expect("agent dir");
    std::fs::write(cwd.join(".maho/mcp.json"), r#"{"mcpServers":{"context7":{"url":"https://example.invalid/mcp"}}}"#).expect("project mcp.json");

    let mut config = load(&cwd, &agent_dir);
    assert_eq!(config.servers[CONTEXT7_SERVER_NAME].state, McpServerState::Untrusted);
    merge_extension_mcp_servers(&mut config, &declarations(None)).expect("merge");

    let context7 = &config.servers[CONTEXT7_SERVER_NAME];
    assert_eq!(context7.source, McpServerSource::Extension);
    assert_eq!(context7.state, McpServerState::Enabled);
    assert_eq!(context7.config.as_ref().and_then(|server| server.url.clone()).as_deref(), Some("https://mcp.context7.com/mcp"));

    let diagnostics = config.diagnostics.join("\n");
    assert!(diagnostics.contains("replaces untrusted"), "{diagnostics}");
}

#[test]
fn given_a_disabled_global_entry_when_merged_then_the_extension_declaration_is_skipped_entirely() {
    let root = tempfile::tempdir().expect("temp");
    let cwd = root.path().join("project");
    let agent_dir = root.path().join("agent");
    std::fs::create_dir_all(&cwd).expect("cwd");
    std::fs::create_dir_all(&agent_dir).expect("agent dir");
    std::fs::write(agent_dir.join("mcp.json"), r#"{"mcpServers":{"grep_app":{"url":"http://127.0.0.1:9/mcp","enabled":true}}}"#).expect("global mcp.json");

    let mut config = load(&cwd, &agent_dir);
    merge_extension_mcp_servers(&mut config, &declarations(None)).expect("merge");

    let grep_app = &config.servers[GREP_APP_SERVER_NAME];
    assert_eq!(grep_app.source, McpServerSource::Global);
    assert_eq!(grep_app.config.as_ref().and_then(|server| server.url.clone()).as_deref(), Some("http://127.0.0.1:9/mcp"));
    assert!(config.diagnostics.iter().any(|diagnostic| diagnostic.contains("Extension MCP server 'grep_app'")));
}

#[test]
fn given_the_two_declarations_when_inspected_then_neither_carries_headers_or_a_literal_endpoint_override() {
    for declaration in declarations(Some("ctx7sk-live-secret")) {
        let McpServerDeclaration { headers, url, command, args, .. } = declaration.config;
        assert_eq!(headers, None);
        assert_eq!(command, None);
        assert_eq!(args, None);
        assert!(url.is_some_and(|url| url.starts_with("https://mcp.")));
    }
}
