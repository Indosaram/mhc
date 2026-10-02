use std::{collections::BTreeMap, fs, path::Path};
use maho_ext_mcp::{config::*, config_schema::*};
use serde_json::{json, Value};
fn write(path: &Path, data: Value) {
    fs::create_dir_all(path.parent().expect("fixture has parent")).expect("create fixture directory");
    fs::write(path, data.to_string()).expect("write fixture config");
}
fn load(root: &Path, trusted: bool, env: &BTreeMap<String, String>) -> Result<ResolvedMcpConfig, McpConfigValidationError> {
    load_mcp_config(LoadMcpConfigOptions { cwd: root, agent_dir: &root.join("agent"), env, project_trusted: trusted })
}
#[test]
fn trusted_configs_merge_and_interpolate() {
    let root = tempfile::tempdir().unwrap();
    write(&root.path().join("agent/mcp.json"), json!({"settings":{"importConfigs":["claude"],"toolPrefix":"global"},"mcpServers":{"shared":{"command":"global"},"http":{"url":"https://example.test/mcp","headers":{"Authorization":"Bearer ${TOKEN}"}}}}));
    write(&root.path().join(".mcp.json"), json!({"mcpServers":{"claude":{"command":"claude","args":["${ABSENT:-fallback}"]}}}));
    write(&root.path().join(".maho/mcp.json"), json!({"settings":{"toolPrefix":"project"},"mcpServers":{"shared":{"command":"project","args":["${TOKEN}"]}}}));
    let result = load(root.path(), true, &BTreeMap::from([("TOKEN".into(),"value".into())])).unwrap();
    assert_eq!(result.settings.tool_prefix.as_deref(), Some("project"));
    assert_eq!(result.servers["shared"].config.as_ref().unwrap().args.as_ref().unwrap(), &["value"]);
    assert_eq!(result.servers["claude"].config.as_ref().unwrap().args.as_ref().unwrap(), &["fallback"]);
    assert_eq!(result.servers["http"].transport, Some(Transport::Http));
}
#[test]
fn claude_config_requires_opt_in() {
    let root = tempfile::tempdir().unwrap();
    write(&root.path().join(".mcp.json"), json!({"mcpServers":{"claude":{"command":"claude"}}}));
    assert!(load(root.path(), true, &BTreeMap::new()).unwrap().servers.is_empty());
}
#[test]
fn invalid_arrays_are_rejected() {
    assert_eq!(validate_raw(json!({"mcpServers":{"x":{"command":"node","args":"not-array"}}})).unwrap_err().to_string(), "Invalid MCP config at mcpServers.x.args: Expected array");
}
#[test]
fn typebox_boundary_errors_preserve_machine_consumed_paths_and_kinds() {
    for (value,path,message) in [
        (json!({"mcpServers":{"x":{"command":1}}}),"mcpServers.x.command","must be string"),
        (json!({"mcpServers":{"x":{"command":"node","auth":true}}}),"mcpServers.x.auth","must be string"),
        (json!({"mcpServers":{"x":{"command":"node","oauth":{"foo":1}}}}),"mcpServers.x.oauth.foo","schema is false"),
        (json!({"settings":{"outputGuard":{"maxBytes":"ten"}}}),"settings.outputGuard.maxBytes","must be number"),
    ] {assert_eq!(validate_raw(value).unwrap_err().to_string(),format!("Invalid MCP config at {path}: {message}"));}
    assert!(validate_raw(json!({"mcpServers":{"x":{"command":"node","connectTimeoutMs":-1}},"settings":{"searchThreshold":1.5}})).is_ok());
}
#[test]
fn hashes_ignore_key_order_and_startup_policy() {
    let a = normalize_server(serde_json::from_value(json!({"command":"node","env":{"A":"1","B":"2"}})).unwrap());
    let mut b = a.clone(); b.startup_timeout_ms = Some(9000.0);
    assert_eq!(hash_config(&a).unwrap(), hash_config(&b).unwrap());
}
#[test]
fn future_fields_round_trip() {
    let raw = json!({"settings":{"oauthCallbackUrl":"https://example.test/cb","stubSwap":false,"nativeToolSearch":true},"mcpServers":{"x":{"command":"node","auth":"oauth","oauth":{"clientId":"id","flow":"code","scopes":["read"]}}}});
    assert_eq!(serde_json::to_value(validate_raw(raw.clone()).unwrap()).unwrap(), raw);
}
#[test]
fn callback_port_must_fit_tcp_range() {
    for port in [json!(-1),json!(65536),json!(1.5)] {
        assert!(validate_raw(json!({"mcpServers":{"x":{"url":"https://example.test","oauth":{"callbackPort":port}}}})).is_err());
    }
}
#[test]
fn keep_alive_is_supported() {
    let raw = validate_raw(json!({"mcpServers":{"x":{"command":"node","lifecycle":"keep-alive"}}})).unwrap();
    assert_eq!(raw.mcp_servers.unwrap()["x"].lifecycle, Some(Lifecycle::KeepAlive));
}
#[test]
fn default_timeouts_are_normalized() {
    let config = normalize_server(ServerConfigWire::default());
    assert_eq!(config.idle_timeout_min,Some(10.0));
    assert_eq!(config.request_timeout_ms,Some(30000.0));
    assert_eq!(config.startup_timeout_ms,Some(250.0));
}
#[test]
fn boolean_direct_tools_is_supported() {
    let raw = validate_raw(json!({"mcpServers":{"x":{"command":"node","directTools":true}}})).unwrap();
    assert_eq!(raw.mcp_servers.unwrap()["x"].direct_tools,Some(DirectTools::All(true)));
}
#[test]
fn output_guard_fields_round_trip() {
    let settings = json!({"outputGuard":{"maxBytes":1000,"maxLines":42,"maxTokens":200}});
    let raw = validate_raw(json!({"settings":settings})).unwrap();
    assert_eq!(raw.settings.unwrap().output_guard.unwrap().max_lines,Some(42.0));
}
#[test]
fn extension_server_preserves_exposure_and_defaults_cwd() {
    let raw = serde_json::from_value(json!({"command":"node","exposure":"direct"})).unwrap();
    let result = resolve_extension_mcp_server("fresh",raw,Path::new("<ext>"),Path::new("/tmp/ext")).unwrap();
    let config = result.config.unwrap();
    assert_eq!(config.cwd.as_deref(),Some("/tmp/ext")); assert_eq!(config.exposure,Some(Exposure::Direct));
}
#[test]
fn skill_server_starts_with_zero_active_tools() {
    let raw = serde_json::from_value(json!({"command":"node","directTools":true})).unwrap();
    let config = resolve_skill_mcp_server("skill",raw,Path::new("skill")).unwrap().config.unwrap();
    assert_eq!(config.exposure,Some(Exposure::Search)); assert_eq!(config.direct_tools,Some(DirectTools::Patterns(Vec::new())));
}
#[test]
fn command_substitution_is_rejected() {
    for value in [" !curl example", "$(touch forbidden)"] {
        assert!(interpolate_value(&json!(value),"mcp.mcpServers.x.command",&BTreeMap::new()).is_err());
    }
}
#[test]
fn missing_endpoints_are_rejected() {
    for server in [json!({}),json!({"type":"http"})] {
        assert!(validate_raw(json!({"mcpServers":{"x":server}})).is_err());
    }
}
#[test]
fn empty_interpolated_command_is_rejected() {
    let root = tempfile::tempdir().unwrap();
    write(&root.path().join("agent/mcp.json"), json!({"mcpServers":{"x":{"command":"${MISSING}"}}}));
    assert!(load(root.path(),true,&BTreeMap::new()).is_err());
}
#[test]
fn empty_interpolated_url_is_rejected() {
    let root = tempfile::tempdir().unwrap();
    write(&root.path().join("agent/mcp.json"), json!({"mcpServers":{"x":{"type":"http","url":"${MISSING:-}"}}}));
    assert!(load(root.path(),true,&BTreeMap::new()).is_err());
}
#[test]
fn disabled_placeholder_is_valid() {
    assert!(validate_raw(json!({"mcpServers":{"x":{"enabled":false}}})).is_ok());
}
#[test]
fn untrusted_project_is_not_interpolated_or_spawnable() {
    let root = tempfile::tempdir().unwrap();
    write(&root.path().join("agent/mcp.json"),json!({"mcpServers":{"global":{"command":"node"}}}));
    write(&root.path().join(".maho/mcp.json"),json!({"mcpServers":{"project":{"command":"$(forbidden)"}}}));
    let result = load(root.path(),false,&BTreeMap::new()).unwrap();
    let mut names = Vec::new(); visit_spawnable_mcp_servers(&result,|n,_| names.push(n.to_owned()));
    assert_eq!(names,["global"]); assert!(result.servers["project"].config.is_none());
}
#[test]
fn untrusted_sources_cannot_shadow_global() {
    let root = tempfile::tempdir().unwrap();
    write(&root.path().join("agent/mcp.json"),json!({"settings":{"importConfigs":["claude"]},"mcpServers":{"x":{"command":"global"}}}));
    for path in [".mcp.json",".maho/mcp.json"] { write(&root.path().join(path),json!({"mcpServers":{"x":{"command":"untrusted"}}})); }
    let result = load(root.path(),false,&BTreeMap::new()).unwrap();
    assert_eq!(result.servers["x"].source,McpServerSource::Global); assert_eq!(result.diagnostics.len(),2);
}
#[test]
fn malformed_untrusted_config_is_diagnostic() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join(".maho")).unwrap(); fs::write(root.path().join(".maho/mcp.json"),"{").unwrap();
    assert_eq!(load(root.path(),false,&BTreeMap::new()).unwrap().diagnostics.len(),1);
}
#[test]
fn trusted_project_can_spawn() {
    let root = tempfile::tempdir().unwrap();
    write(&root.path().join(".maho/mcp.json"),json!({"mcpServers":{"x":{"command":"${COMMAND}"}}}));
    let result = load(root.path(),true,&BTreeMap::from([("COMMAND".into(),"node".into())])).unwrap();
    assert_eq!(result.servers["x"].config.as_ref().unwrap().command.as_deref(),Some("node"));
}
fn declaration(name: &str) -> maho_ext_api::RegisteredMcpServerDeclaration {
    maho_ext_api::RegisteredMcpServerDeclaration { name: name.into(), config: maho_ext_api::McpServerDeclaration { command: Some("extension".into()), exposure: Some(maho_ext_api::McpExposure::Direct), ..Default::default() }, extension_path: "<ext>".into(), registration_cwd: "/tmp/ext".into() }
}
#[test]
fn trusted_global_wins_over_extension() {
    let root = tempfile::tempdir().unwrap();
    write(&root.path().join("agent/mcp.json"),json!({"mcpServers":{"x":{"command":"global"}}}));
    let mut result = load(root.path(),true,&BTreeMap::new()).unwrap();
    merge_extension_mcp_servers(&mut result,&[declaration("x")]).unwrap();
    assert_eq!(result.servers["x"].source,McpServerSource::Global);
}
#[test]
fn disabled_global_wins_over_extension() {
    let root = tempfile::tempdir().unwrap();
    write(&root.path().join("agent/mcp.json"),json!({"mcpServers":{"x":{"enabled":false}}}));
    let mut result = load(root.path(),true,&BTreeMap::new()).unwrap();
    merge_extension_mcp_servers(&mut result,&[declaration("x")]).unwrap();
    assert_eq!(result.servers["x"].state,McpServerState::Disabled);
}
#[test]
fn extension_replaces_untrusted_placeholder() {
    let root = tempfile::tempdir().unwrap();
    write(&root.path().join(".maho/mcp.json"),json!({"mcpServers":{"x":{"command":"evil"}}}));
    let mut result = load(root.path(),false,&BTreeMap::new()).unwrap();
    merge_extension_mcp_servers(&mut result,&[declaration("x")]).unwrap();
    assert_eq!(result.servers["x"].source,McpServerSource::Extension); assert_eq!(result.diagnostics.len(),1);
}
#[test]
fn identical_declarations_have_stable_hashes() {
    let root = tempfile::tempdir().unwrap();
    let mut result = load(root.path(),true,&BTreeMap::new()).unwrap();
    merge_extension_mcp_servers(&mut result,&[declaration("x")]).unwrap();
    let first = result.servers.remove("x").unwrap();
    merge_extension_mcp_servers(&mut result,&[declaration("x")]).unwrap();
    assert_eq!(first.config_hash,result.servers["x"].config_hash);
}
