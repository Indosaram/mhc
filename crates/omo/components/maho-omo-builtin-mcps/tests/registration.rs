mod support;

use std::collections::BTreeMap;

use maho_ext_api::{Extension, McpAuth, McpExposure, McpLifecycle, McpTransport};
use maho_ext_host::ExtensionRunner;
use maho_omo_builtin_mcps::{
    BUILTIN_MCPS_COMPONENT_NAME, BuiltinMcpsComponent, CONTEXT7_API_KEY_ENV, CONTEXT7_SERVER_NAME,
    GREP_APP_SERVER_NAME,
};

fn component(api_key: Option<&str>) -> BuiltinMcpsComponent {
    let env: BTreeMap<String, String> = api_key
        .map(|value| BTreeMap::from([(CONTEXT7_API_KEY_ENV.to_owned(), value.to_owned())]))
        .unwrap_or_default();
    BuiltinMcpsComponent::from_env(&env)
}

#[test]
fn given_the_component_name_when_compared_then_it_is_upstreams() {
    assert_eq!(BUILTIN_MCPS_COMPONENT_NAME, "builtin-mcps");
    assert_eq!(BuiltinMcpsComponent::NAME, BUILTIN_MCPS_COMPONENT_NAME);
}

#[test]
fn given_no_context7_api_key_when_the_component_registers_then_the_real_api_holds_both_declarations_in_order() {
    let root = tempfile::tempdir().expect("temp");
    let mut api = support::api(root.path());

    component(None).register(&mut api);

    let declared = &api.registered.mcp_servers;
    assert_eq!(declared.iter().map(|entry| entry.name.as_str()).collect::<Vec<_>>(), [CONTEXT7_SERVER_NAME, GREP_APP_SERVER_NAME]);
    assert_eq!(declared[0].extension_path, "<builtin:omo>");
    assert_eq!(declared[0].registration_cwd.as_path(), root.path());
    assert_eq!(declared[0].config.transport, Some(McpTransport::Http));
    assert_eq!(declared[0].config.auth, Some(McpAuth::Disabled));
    assert_eq!(declared[0].config.lifecycle, Some(McpLifecycle::Lazy));
    assert_eq!(declared[0].config.exposure, Some(McpExposure::Search));
    assert_eq!(declared[1].config.auth, Some(McpAuth::Disabled));
    assert_eq!(declared[1].config.exposure, Some(McpExposure::Search));
}

#[test]
fn given_a_real_context7_api_key_when_the_component_registers_then_only_the_env_name_reaches_the_host() {
    let root = tempfile::tempdir().expect("temp");
    let mut api = support::api(root.path());

    component(Some("ctx7sk-live-secret")).register(&mut api);

    let context7 = &api.registered.mcp_servers[0];
    assert_eq!(context7.name, CONTEXT7_SERVER_NAME);
    assert_eq!(context7.config.auth, Some(McpAuth::Bearer));
    assert_eq!(context7.config.bearer_token_env.as_deref(), Some(CONTEXT7_API_KEY_ENV));
    assert_eq!(context7.config.headers, None);
    assert!(!format!("{:?}", api.registered.mcp_servers).contains("ctx7sk-live-secret"));
}

#[test]
fn given_the_component_when_it_registers_then_it_writes_no_config_file_into_the_agent_directory() {
    let root = tempfile::tempdir().expect("temp");
    let agent_dir = root.path().join("agent");
    std::fs::create_dir_all(&agent_dir).expect("agent dir");
    let mut api = support::api(root.path());
    api.registered.source_info = maho_ext_api::SourceInfo { path: "<builtin:omo>".to_owned(), source: "builtin".to_owned(), ..Default::default() };

    component(None).register(&mut api);

    let entries: Vec<String> = std::fs::read_dir(&agent_dir)
        .expect("read agent dir")
        .map(|entry| entry.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    assert!(entries.is_empty(), "extension declarations are runtime-only: {entries:?}");
    assert!(!agent_dir.join("mcp.json").exists());
}

#[tokio::test]
async fn given_a_real_runner_when_the_component_registers_then_the_context_carries_both_declarations_to_the_mcp_extension() {
    let root = tempfile::tempdir().expect("temp");
    let runner = ExtensionRunner::from_static(vec![Box::new(component(None))], support::context(root.path()));

    let declared = runner.get_registered_mcp_servers();
    assert_eq!(declared.iter().map(|entry| entry.name.as_str()).collect::<Vec<_>>(), [CONTEXT7_SERVER_NAME, GREP_APP_SERVER_NAME]);

    let context = runner.create_context().expect("context");
    assert_eq!(context.registered_mcp_servers, declared, "the field maho-ext-mcp attach_session reads");
    assert_eq!(context.get_registered_mcp_servers(), declared);
}
