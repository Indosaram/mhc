use maho_ext_mcp::{skills::*,auth::context::*,config_schema::*};
use serde_json::json;
#[test]
fn sidecar_wins_over_frontmatter() {
    let root=tempfile::tempdir().unwrap();
    let path=root.path().join("SKILL.md");std::fs::write(&path,"---\nmcp:\n  loser:\n    command: node\n---\nBody").unwrap();
    std::fs::write(root.path().join("mcp.json"),json!({"winner":{"command":"node"}}).to_string()).unwrap();
    let declarations=parse_skill_mcp_declarations(&[SkillLike {name:"skill".into(),file_path:path,base_dir:root.path().into()}]);
    assert_eq!(declarations.servers.keys().collect::<Vec<_>>(),vec!["winner"]);
}
#[test]
fn shared_server_preserves_first_config_and_per_skill_filters() {
    let root=tempfile::tempdir().unwrap();let mut skills=Vec::new();
    for (name,tool) in [("a","tool_1"),("b","tool_2*")] {let dir=root.path().join(name);std::fs::create_dir(&dir).unwrap();std::fs::write(dir.join("mcp.json"),json!({"shared":{"command":name,"includeTools":[tool]}}).to_string()).unwrap();skills.push(SkillLike {name:name.into(),file_path:dir.join("SKILL.md"),base_dir:dir});}
    let declarations=parse_skill_mcp_declarations(&skills);
    let registered=[RegisteredSkillTool {name:"mcp_shared_tool_1".into(),tool_name:"tool_1".into(),server:"shared".into()},RegisteredSkillTool {name:"mcp_shared_tool_2".into(),tool_name:"tool_2".into(),server:"shared".into()}];
    assert_eq!(declarations.servers["shared"].raw["command"],"a");
    assert_eq!(skill_activation_targets(&declarations,"b",&registered),vec!["mcp_shared_tool_2"]);
}
#[test]
fn frontmatter_only_servers_are_discovered() {
    let root=tempfile::tempdir().unwrap();let path=root.path().join("SKILL.md");std::fs::write(&path,"---\nmcp:\n  server:\n    command: node\n---\nBody").unwrap();
    let declarations=parse_skill_mcp_declarations(&[SkillLike {name:"skill".into(),file_path:path,base_dir:root.path().into()}]);
    assert!(declarations.servers.contains_key("server"));assert!(declarations.warnings.is_empty());
}
#[test]
fn auth_mode_respects_overrides_headers_and_transport() {
    let mut config=McpServerConfig {transport:Some(Transport::Http),url:Some("https://example.test".into()),..Default::default()};
    assert_eq!(resolve_auth_mode(&config),ServerAuthMode::OAuth);
    config.headers=Some(std::collections::BTreeMap::from([("Authorization".into(),"fixture".into())]));assert_eq!(resolve_auth_mode(&config),ServerAuthMode::None);
    config.bearer_token_env=Some("TOKEN".into());assert_eq!(resolve_auth_mode(&config),ServerAuthMode::Bearer);
    config.auth=Some(Auth::Disabled(false));assert_eq!(resolve_auth_mode(&config),ServerAuthMode::None);
}
#[test]
fn secret_warning_contains_fingerprint_not_header_value() {
    let config=McpServerConfig {headers:Some(std::collections::BTreeMap::from([("Authorization".into(),"Bearer fixture-secret".into())])),..Default::default()};
    let warnings=detect_literal_bearer_warnings("srv",&config).unwrap();assert_eq!(warnings.len(),1);assert!(!warnings[0].contains("fixture-secret"));
}
#[test]
fn array_wrapped_server_map_uses_numeric_entry_names() {
    let root=tempfile::tempdir().unwrap();let path=root.path().join("SKILL.md");
    std::fs::write(root.path().join("mcp.json"),json!({"mcpServers":[{"command":"node"},null,[],{"url":"https://example.test"}]}).to_string()).unwrap();
    let declarations=parse_skill_mcp_declarations(&[SkillLike {name:"skill".into(),file_path:path,base_dir:root.path().into()}]);
    assert_eq!(declarations.servers.keys().cloned().collect::<Vec<_>>(),vec!["0","3"]);
    assert_eq!(declarations.servers["0"].raw["command"],"node");
}
