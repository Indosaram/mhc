//! Tests written from docs/cleanroom/specs/mcp.md §5.

use maho_config::Env;
use maho_ext_api::{
    EventBus, Extension, ExtensionApi, ExtensionRuntime, ExtensionSessionProfile, LoadedExtension,
    McpAuth, McpLifecycle, McpTransport, RegisteredMcpServerDeclaration, SourceInfo,
};
use maho_mcp_builtins::{BuiltinSettings, McpBuiltinsExtension, declarations};
use std::collections::BTreeMap;
use std::path::Path;

fn names(settings: &BuiltinSettings) -> Vec<&'static str> {
    declarations(settings)
        .into_iter()
        .map(|(name, _)| name)
        .collect()
}

fn env(home: &Path, extra: &[(&str, &str)]) -> Env {
    let mut env = Env::new();
    env.insert("HOME".into(), home.to_string_lossy().into_owned());
    for (key, value) in extra {
        env.insert((*key).into(), (*value).into());
    }
    env
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, text).expect("write");
}

#[test]
fn both_servers_are_declared_by_default() {
    let declared = declarations(&BuiltinSettings::default());
    assert_eq!(
        declared.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
        ["context7", "grep_app"]
    );
    let (_, context7) = &declared[0];
    assert_eq!(
        context7.url.as_deref(),
        Some("https://mcp.context7.com/mcp")
    );
    assert_eq!(context7.transport, Some(McpTransport::Http));
    assert_eq!(context7.lifecycle, Some(McpLifecycle::Lazy));
    assert_eq!(context7.auth, Some(McpAuth::Disabled));
    assert_eq!(context7.bearer_token_env, None);
    assert_eq!(declared[1].1.url.as_deref(), Some("https://mcp.grep.app"));
}

#[test]
fn context7_key_turns_on_bearer_auth() {
    let dir = tempfile::tempdir().expect("dir");
    let settings = BuiltinSettings::resolve(
        dir.path(),
        &env(dir.path(), &[("CONTEXT7_API_KEY", "secret")]),
    );
    let declared = declarations(&settings);
    assert_eq!(declared[0].1.auth, Some(McpAuth::Bearer));
    assert_eq!(
        declared[0].1.bearer_token_env.as_deref(),
        Some("CONTEXT7_API_KEY")
    );
    assert_eq!(declared[1].1.auth, Some(McpAuth::Disabled));
}

#[test]
fn switch_offline_and_config_turn_them_off() {
    let dir = tempfile::tempdir().expect("dir");
    for (key, value) in [
        ("MAHO_BUILTIN_MCPS", "0"),
        ("MAHO_BUILTIN_MCPS", "Off"),
        ("PI_OFFLINE", "1"),
        ("MAHO_OFFLINE", "true"),
    ] {
        let settings = BuiltinSettings::resolve(dir.path(), &env(dir.path(), &[(key, value)]));
        assert!(names(&settings).is_empty(), "{key}={value}");
    }
    let on = BuiltinSettings::resolve(dir.path(), &env(dir.path(), &[("MAHO_BUILTIN_MCPS", "1")]));
    assert_eq!(names(&on), ["context7", "grep_app"]);

    write(
        &dir.path().join(".omo/omo.jsonc"),
        r#"{"mcp": {"builtins": false}}"#,
    );
    let off = BuiltinSettings::resolve(dir.path(), &env(dir.path(), &[]));
    assert!(names(&off).is_empty());
}

#[test]
fn disabled_builtins_union_across_layers() {
    let dir = tempfile::tempdir().expect("dir");
    let project = dir.path().join("proj");
    write(
        &dir.path().join(".omo/omo.jsonc"),
        r#"{"mcp": {"disabled_builtins": ["context7"]}}"#,
    );
    write(
        &project.join(".omo/omo.jsonc"),
        r#"{"mcp": {"disabled_builtins": ["unknown"]}}"#,
    );
    let settings = BuiltinSettings::resolve(&project, &env(dir.path(), &[]));
    assert_eq!(names(&settings), ["grep_app"]);
}

#[test]
fn extension_registers_the_declarations() {
    let dir = tempfile::tempdir().expect("dir");
    let mut api = ExtensionApi::new(
        LoadedExtension::new(
            "<builtin:mcp-builtins>",
            dir.path().into(),
            SourceInfo::default(),
        ),
        ExtensionSessionProfile::default(),
        EventBus::default(),
        ExtensionRuntime::default(),
    );
    McpBuiltinsExtension.register(&mut api);
    // The process environment of the test runner decides (offline/switch); whatever is
    // registered must be a subset of the builtin list with valid endpoints.
    for declaration in &api.registered.mcp_servers {
        assert!(["context7", "grep_app"].contains(&declaration.name.as_str()));
        assert!(
            declaration
                .config
                .url
                .as_deref()
                .is_some_and(|url| url.starts_with("https://"))
        );
    }
}

#[test]
fn user_mcp_json_entry_wins_over_a_builtin() {
    let dir = tempfile::tempdir().expect("dir");
    let agent = dir.path().join("agent");
    write(
        &agent.join("mcp.json"),
        r#"{"mcpServers": {"context7": {"command": "my-context7", "args": []}}}"#,
    );
    let env_map: BTreeMap<String, String> = BTreeMap::new();
    let mut config =
        maho_ext_mcp::config::load_mcp_config(maho_ext_mcp::config_schema::LoadMcpConfigOptions {
            cwd: dir.path(),
            agent_dir: &agent,
            env: &env_map,
            project_trusted: false,
            home_dir: Some(dir.path()),
            claude_config_dir: None,
        })
        .expect("config");
    let registered: Vec<RegisteredMcpServerDeclaration> = declarations(&BuiltinSettings::default())
        .into_iter()
        .map(|(name, config)| RegisteredMcpServerDeclaration {
            name: name.into(),
            config,
            extension_path: "<builtin:mcp-builtins>".into(),
            registration_cwd: dir.path().into(),
        })
        .collect();
    maho_ext_mcp::config::merge_extension_mcp_servers(&mut config, &registered).expect("merge");
    let context7 = &config.servers["context7"];
    assert_eq!(
        context7.source,
        maho_ext_mcp::config_schema::McpServerSource::Global
    );
    assert!(
        config
            .diagnostics
            .iter()
            .any(|d| d.contains("Extension MCP server 'context7'") && d.contains("wins")),
        "{:?}",
        config.diagnostics
    );
    let grep = &config.servers["grep_app"];
    assert_eq!(
        grep.source,
        maho_ext_mcp::config_schema::McpServerSource::Extension
    );
    assert_eq!(
        grep.state,
        maho_ext_mcp::config_schema::McpServerState::Enabled
    );
}
