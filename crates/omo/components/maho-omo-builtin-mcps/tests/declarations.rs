use std::collections::BTreeMap;

use maho_ext_api::{McpAuth, McpExposure, McpLifecycle, McpTransport};
use maho_omo_builtin_mcps::{
    CONTEXT7_API_KEY_ENV, CONTEXT7_SERVER_NAME, CONTEXT7_URL, DEFERRED_EXPOSURE, GREP_APP_SERVER_NAME,
    GREP_APP_URL, builtin_mcp_declarations, create_context7_declaration, grep_app_declaration,
    has_context7_api_key,
};

fn env(api_key: Option<&str>) -> BTreeMap<String, String> {
    api_key.map(|value| BTreeMap::from([(CONTEXT7_API_KEY_ENV.to_owned(), value.to_owned())])).unwrap_or_default()
}

fn wire_json(declaration: &maho_ext_api::McpServerDeclaration) -> String {
    serde_json::to_string(&maho_ext_mcp::config_schema::ServerConfigWire::from(declaration)).expect("wire json")
}

#[test]
fn given_no_context7_api_key_when_registered_then_declares_the_anonymous_lazy_search_context7_and_grep_app_http_servers() {
    let declarations = builtin_mcp_declarations(&env(None));

    assert_eq!(declarations.len(), 2);
    assert_eq!(declarations[0].0, CONTEXT7_SERVER_NAME);
    assert_eq!(declarations[1].0, GREP_APP_SERVER_NAME);

    let context7 = &declarations[0].1;
    assert_eq!(context7.transport, Some(McpTransport::Http));
    assert_eq!(context7.url.as_deref(), Some(CONTEXT7_URL));
    assert_eq!(context7.enabled, Some(true));
    assert_eq!(context7.auth, Some(McpAuth::Disabled));
    assert_eq!(context7.bearer_token_env, None);
    assert_eq!(context7.lifecycle, Some(McpLifecycle::Lazy));
    assert_eq!(context7.exposure, Some(McpExposure::Search));
    assert_eq!(DEFERRED_EXPOSURE, McpExposure::Search);

    let grep_app = &declarations[1].1;
    assert_eq!(grep_app.transport, Some(McpTransport::Http));
    assert_eq!(grep_app.url.as_deref(), Some(GREP_APP_URL));
    assert_eq!(grep_app.enabled, Some(true));
    assert_eq!(grep_app.auth, Some(McpAuth::Disabled));
    assert_eq!(grep_app.lifecycle, Some(McpLifecycle::Lazy));
    assert_eq!(grep_app.exposure, Some(McpExposure::Search));

    assert_eq!(grep_app_declaration(), grep_app.clone());
}

#[test]
fn given_a_real_context7_api_key_when_registered_then_context7_authenticates_through_bearer_token_env_without_inlining_the_token() {
    let key = "ctx7sk-live-secret";
    let declaration = create_context7_declaration(&env(Some(key)));

    assert_eq!(declaration.auth, Some(McpAuth::Bearer));
    assert_eq!(declaration.bearer_token_env.as_deref(), Some(CONTEXT7_API_KEY_ENV));
    assert_eq!(declaration.headers, None);
    assert!(!wire_json(&declaration).contains(key), "the declaration never carries the literal token");
    assert!(!format!("{declaration:?}").contains(key), "nor does its debug form");
}

#[test]
fn given_a_placeholder_context7_api_key_when_registered_then_context7_stays_anonymous() {
    for placeholder in ["", "   ", "<YOUR_API_KEY>", "your-api-key", "\"Your API Key\"", "your_api_key", "YOUR-API-KEY"] {
        assert!(!has_context7_api_key(Some(placeholder)), "{placeholder:?}");
        let declaration = create_context7_declaration(&env(Some(placeholder)));
        assert_eq!(declaration.auth, Some(McpAuth::Disabled), "{placeholder:?}");
        assert_eq!(declaration.bearer_token_env, None, "{placeholder:?}");
    }
}

#[test]
fn given_an_absent_or_real_context7_api_key_when_classified_then_only_a_real_value_authenticates() {
    assert!(!has_context7_api_key(None));
    assert!(has_context7_api_key(Some("ctx7sk-live-secret")));
    assert!(has_context7_api_key(Some("  ctx7sk-live-secret  ")));
    assert!(!has_context7_api_key(Some("your api key")));
    // Upstream rewrites a trailing separator run to a space, so these are NOT the placeholder.
    assert!(has_context7_api_key(Some("your api key-")));
    assert!(has_context7_api_key(Some("your-api-key!")));
}

#[test]
fn given_a_context7_declaration_when_serialized_for_a_config_dump_then_no_secret_value_appears() {
    let declarations = builtin_mcp_declarations(&env(Some("ctx7sk-live-secret")));
    let dumped = declarations.iter().map(|(_, declaration)| wire_json(declaration)).collect::<Vec<_>>().join("\n");

    assert!(dumped.contains("bearerTokenEnv"));
    assert!(dumped.contains(CONTEXT7_API_KEY_ENV));
    assert!(!dumped.contains("ctx7sk-live-secret"));
}
