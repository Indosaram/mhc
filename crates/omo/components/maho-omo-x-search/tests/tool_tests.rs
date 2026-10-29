//! Port of omo-senpi `components/x-search/tool.test.ts` at latest `455dee62`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use maho_ext_api::{
    AgentToolResult, BuildSystemPromptOptions, ExtensionContext, ExtensionFuture, ExtensionMode,
    ExtensionUi, ExtensionUiDialogOptions, Model, ModelRegistry, NotificationType, SessionEntry,
    SessionManager, Theme, ToolExecutionMode, ToolExposure, ToolSessionManager, UiFuture,
};
use maho_omo_x_search::client::{
    DEFAULT_CARRIER, XSearchFetch, XSearchHttpResponse, carrier_model,
};
use maho_omo_x_search::format::XSearchErrorCode;
use maho_omo_x_search::tool::{
    CreateXSearchToolOptions, X_SEARCH_MODEL_ENV, X_SEARCH_TOOL_NAME, XSearchToolDetails,
    create_x_search_tool,
};
use serde_json::{Value, json};

fn probe_fixture() -> Value {
    let content = include_str!("./__fixtures__/x-search-response.probe.json");
    serde_json::from_str(content).expect("parse probe fixture")
}

struct MockModelRegistry {
    stored_cred_type: Option<maho_ai::auth::types::CredentialType>,
    api_key: Option<String>,
}

impl ModelRegistry for MockModelRegistry {
    fn get_all(&self) -> Vec<Model> {
        Vec::new()
    }
    fn get_available(&self) -> Vec<Model> {
        Vec::new()
    }
    fn find(&self, _provider: &str, _id: &str) -> Option<Model> {
        None
    }
    fn has_configured_auth(&self, _model: &Model) -> bool {
        false
    }
    fn get_api_key_for_provider<'a>(
        &'a self,
        _provider: &'a str,
    ) -> ExtensionFuture<'a, Option<String>> {
        Box::pin(async { Ok(None) })
    }
    fn get_stored_credential_type(
        &self,
        provider: &str,
    ) -> Result<Option<maho_ai::auth::types::CredentialType>, maho_ext_api::ExtensionFailure> {
        if provider == "xai" {
            Ok(self.stored_cred_type)
        } else {
            Ok(None)
        }
    }
    fn get_provider_auth<'a>(
        &'a self,
        provider: &'a str,
    ) -> ExtensionFuture<'a, Option<maho_ai::models::AuthResolution>> {
        let key = self.api_key.clone();
        Box::pin(async move {
            if provider == "xai" {
                Ok(key.map(|k| maho_ai::models::AuthResolution {
                    ambient: false,
                    auth: maho_ai::models::ProviderAuthResult {
                        api_key: Some(k),
                        ..Default::default()
                    },
                    env: None,
                }))
            } else {
                Ok(None)
            }
        })
    }
}

struct DummySession;
impl ToolSessionManager for DummySession {
    fn session_id(&self) -> &str {
        "session-1"
    }
    fn session_file(&self) -> Option<&Path> {
        None
    }
}
impl SessionManager for DummySession {
    fn get_entries(&self) -> Vec<SessionEntry> {
        Vec::new()
    }
    fn get_branch(&self) -> Vec<SessionEntry> {
        Vec::new()
    }
    fn get_leaf_id(&self) -> Option<String> {
        None
    }
    fn get_session_name(&self) -> Option<String> {
        None
    }
}

struct DummyUi;
impl ExtensionUi for DummyUi {
    fn select<'a>(
        &'a self,
        _: &'a str,
        _: &'a [String],
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, Option<String>> {
        Box::pin(async { None })
    }
    fn confirm<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, bool> {
        Box::pin(async { false })
    }
    fn input<'a>(
        &'a self,
        _: &'a str,
        _: Option<&'a str>,
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, Option<String>> {
        Box::pin(async { None })
    }
    fn notify(&self, _: &str, _: NotificationType) {}
    fn set_status(&self, _: &str, _: Option<&str>) {}
    fn set_widget(
        &self,
        _: &str,
        _: Option<maho_ext_api::WidgetContent>,
        _: maho_ext_api::ExtensionWidgetOptions,
    ) {
    }
    fn set_header(&self, _: Option<maho_ext_api::ComponentFactory>) {}
    fn set_footer(&self, _: Option<maho_ext_api::ComponentFactory>) {}
    fn set_title(&self, _: &str) {}
    fn paste_to_editor(&self, _: &str) {}
    fn set_editor_text(&self, _: &str) {}
    fn get_editor_text(&self) -> String {
        String::new()
    }
    fn custom(
        &self,
        _: maho_ext_api::ComponentFactory,
        _: maho_ext_api::CustomUiOptions,
    ) -> ExtensionFuture<'_, Value> {
        Box::pin(async { Ok(Value::Null) })
    }
    fn theme(&self) -> Theme {
        Theme::default()
    }
}

fn make_context(
    stored: Option<maho_ai::auth::types::CredentialType>,
    api_key: Option<&str>,
) -> ExtensionContext {
    let registry = Arc::new(MockModelRegistry {
        stored_cred_type: stored,
        api_key: api_key.map(str::to_string),
    });
    ExtensionContext {
        ui: Arc::new(DummyUi),
        mode: ExtensionMode::Print,
        has_ui: false,
        cwd: PathBuf::from("/test"),
        agent_dir: PathBuf::from("/test/agent"),
        session_manager: Arc::new(DummySession),
        model_registry: registry,
        model: None,
        thinking_level: None,
        service_tier: None,
        effective_service_tier: None,
        scoped_models: Vec::new(),
        goal_store_file: None,
        loaded_extension_paths: Vec::new(),
        signal: None,
        steering_signal: None,
        is_idle_fn: Arc::new(|| true),
        wait_for_idle_fn: Arc::new(|| Box::pin(async {})),
        is_project_trusted_fn: Arc::new(|| true),
        is_compacting_fn: Arc::new(|| false),
        get_system_prompt_fn: Arc::new(String::new),
        get_system_prompt_options_fn: Arc::new(BuildSystemPromptOptions::default),
        registered_mcp_servers: Vec::new(),
        update_tool_hook_status: None,
        logger: None,
        idle_coordinator: None,
        defer_macrotask: None,
        compaction_signal: Default::default(),
    }
}

fn text_of(result: &AgentToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|c| match c {
            maho_ext_api::ContentBlock::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

fn details_of(result: &AgentToolResult) -> XSearchToolDetails {
    serde_json::from_value(result.details.clone()).expect("parse details")
}

#[test]
fn test_tool_definition_metadata() {
    let (def, _exec) = create_x_search_tool(CreateXSearchToolOptions::default());

    assert_eq!(def.name, X_SEARCH_TOOL_NAME);
    assert_eq!(def.label, "Search X posts");
    assert_eq!(def.exposure, Some(ToolExposure::Search));
    assert_eq!(def.search_group.as_deref(), Some("x-search"));
    assert_eq!(def.allow_lazy_activation, Some(true));
    assert_eq!(def.execution_mode, Some(ToolExecutionMode::Parallel));
    assert_eq!(
        def.search_keywords,
        Some(vec![
            "X posts".to_string(),
            "tweets".to_string(),
            "twitter search".to_string(),
            "xAI live search".to_string(),
            "what people are saying on X".to_string(),
        ])
    );
    assert!(def.prompt_snippet.is_none());
    assert!(def.prompt_guidelines.is_none());

    let sentences: Vec<&str> = def
        .description
        .split(". ")
        .filter(|p| !p.trim().is_empty())
        .collect();
    assert_eq!(sentences.len(), 4);
    assert!(def.description.contains("from_date"));
    assert!(def.description.contains("allowed_x_handles"));
    assert!(def.description.contains("2-3 narrower searches"));
    assert!(def.description.contains("connected xAI account"));
}

#[tokio::test]
async fn test_execute_given_stored_credential_and_probe_fixture() {
    let calls = Arc::new(std::sync::Mutex::new(Vec::<(
        String,
        BTreeMap<String, String>,
        String,
    )>::new()));
    let calls_clone = calls.clone();

    let fetch_impl: XSearchFetch = Arc::new(move |url, headers, body| {
        calls_clone.lock().unwrap().push((url, headers, body));
        let fixture_body = serde_json::to_string(&probe_fixture()).unwrap();
        Box::pin(async move {
            Ok(XSearchHttpResponse {
                status: 200,
                headers: BTreeMap::new(),
                body: fixture_body,
            })
        })
    });

    let (_def, exec) = create_x_search_tool(CreateXSearchToolOptions {
        fetch_impl: Some(fetch_impl),
        env: Some(BTreeMap::new()),
        resolve_bearer: None,
    });

    let ctx = make_context(
        Some(maho_ai::auth::types::CredentialType::OAuth),
        Some("stored-token"),
    );

    let result = exec(
        "call-1",
        json!({ "query": "Grok CLI", "from_date": "2026-09-01" }),
        None,
        None,
        &ctx,
    )
    .await
    .expect("execution succeeds");

    let text = text_of(&result);
    assert!(text.starts_with("x_search results:"));

    let details = details_of(&result);
    assert!(details.queries[0].contains("since:"));
    assert!(!details.results.is_empty());
    assert_eq!(details.usage.x_search_calls, 3);

    let recorded = calls.lock().unwrap();
    assert_eq!(recorded.len(), 1);
    assert_eq!(
        recorded[0].1.get("authorization").map(String::as_str),
        Some("Bearer stored-token")
    );
    let body_json: Value = serde_json::from_str(&recorded[0].2).unwrap();
    assert_eq!(
        body_json.get("model").and_then(Value::as_str),
        Some(carrier_model(DEFAULT_CARRIER).model.as_str())
    );
}

#[tokio::test]
async fn test_execute_carrier_model_env_override() {
    let captured_body = Arc::new(std::sync::Mutex::new(Value::Null));
    let captured_clone = captured_body.clone();

    let fetch_impl: XSearchFetch = Arc::new(move |_url, _headers, body| {
        *captured_clone.lock().unwrap() = serde_json::from_str(&body).unwrap();
        let fixture_body = serde_json::to_string(&probe_fixture()).unwrap();
        Box::pin(async move {
            Ok(XSearchHttpResponse {
                status: 200,
                headers: BTreeMap::new(),
                body: fixture_body,
            })
        })
    });

    let mut env = BTreeMap::new();
    env.insert(X_SEARCH_MODEL_ENV.to_string(), "grok-4.6".to_string());

    let (_def, exec) = create_x_search_tool(CreateXSearchToolOptions {
        fetch_impl: Some(fetch_impl),
        env: Some(env),
        resolve_bearer: None,
    });

    let ctx = make_context(
        Some(maho_ai::auth::types::CredentialType::OAuth),
        Some("stored-token"),
    );

    let _result = exec("call-1", json!({ "query": "Grok CLI" }), None, None, &ctx)
        .await
        .expect("execution succeeds");

    let body = captured_body.lock().unwrap();
    assert_eq!(body.get("model").and_then(Value::as_str), Some("grok-4.6"));
}

#[tokio::test]
async fn test_execute_no_credential_returns_auth_error_without_calling_xai() {
    let fetched = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let fetched_clone = fetched.clone();

    let fetch_impl: XSearchFetch = Arc::new(move |_url, _headers, _body| {
        fetched_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let fixture_body = serde_json::to_string(&probe_fixture()).unwrap();
        Box::pin(async move {
            Ok(XSearchHttpResponse {
                status: 200,
                headers: BTreeMap::new(),
                body: fixture_body,
            })
        })
    });

    let (_def, exec) = create_x_search_tool(CreateXSearchToolOptions {
        fetch_impl: Some(fetch_impl),
        env: Some(BTreeMap::new()),
        resolve_bearer: None,
    });

    let ctx = make_context(None, None);

    let result = exec("call-1", json!({ "query": "Grok CLI" }), None, None, &ctx)
        .await
        .expect("execution succeeds");

    let text = text_of(&result);
    assert!(text.contains("x_search error [AUTH]"));
    assert_eq!(result.is_error, Some(true));
    assert_eq!(fetched.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test]
async fn test_execute_invalid_params_short_circuits() {
    let fetched = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let fetched_clone = fetched.clone();

    let fetch_impl: XSearchFetch = Arc::new(move |_url, _headers, _body| {
        fetched_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let fixture_body = serde_json::to_string(&probe_fixture()).unwrap();
        Box::pin(async move {
            Ok(XSearchHttpResponse {
                status: 200,
                headers: BTreeMap::new(),
                body: fixture_body,
            })
        })
    });

    let (_def, exec) = create_x_search_tool(CreateXSearchToolOptions {
        fetch_impl: Some(fetch_impl),
        env: Some(BTreeMap::new()),
        resolve_bearer: None,
    });

    let ctx = make_context(
        Some(maho_ai::auth::types::CredentialType::OAuth),
        Some("stored-token"),
    );

    let result = exec(
        "call-1",
        json!({
            "query": "Grok CLI",
            "allowed_x_handles": ["a"],
            "excluded_x_handles": ["b"]
        }),
        None,
        None,
        &ctx,
    )
    .await
    .expect("execution succeeds");

    let text = text_of(&result);
    assert!(text.contains("x_search error [INVALID_FILTERS]"));
    let details = details_of(&result);
    assert_eq!(details.code, Some(XSearchErrorCode::InvalidFilters));
    assert_eq!(fetched.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test]
async fn test_execute_upstream_rate_limit_surfaces_code() {
    let fetch_impl: XSearchFetch = Arc::new(|_url, _headers, _body| {
        let mut headers = BTreeMap::new();
        headers.insert("retry-after".to_string(), "7".to_string());
        Box::pin(async move {
            Ok(XSearchHttpResponse {
                status: 429,
                headers,
                body: "{\"error\": \"slow down\"}".to_string(),
            })
        })
    });

    let (_def, exec) = create_x_search_tool(CreateXSearchToolOptions {
        fetch_impl: Some(fetch_impl),
        env: Some(BTreeMap::new()),
        resolve_bearer: None,
    });

    let ctx = make_context(
        Some(maho_ai::auth::types::CredentialType::OAuth),
        Some("stored-token"),
    );

    let result = exec("call-1", json!({ "query": "Grok CLI" }), None, None, &ctx)
        .await
        .expect("execution succeeds");

    let text = text_of(&result);
    assert!(text.contains("x_search error [RATE_LIMITED]"));
    let details = details_of(&result);
    assert_eq!(details.code, Some(XSearchErrorCode::RateLimited));
    assert_eq!(result.is_error, Some(true));
}

#[tokio::test]
async fn test_execute_resolve_bearer_override() {
    let captured_auth = Arc::new(std::sync::Mutex::new(String::new()));
    let captured_clone = captured_auth.clone();

    let fetch_impl: XSearchFetch = Arc::new(move |_url, headers, _body| {
        *captured_clone.lock().unwrap() = headers.get("authorization").cloned().unwrap_or_default();
        let fixture_body = serde_json::to_string(&probe_fixture()).unwrap();
        Box::pin(async move {
            Ok(XSearchHttpResponse {
                status: 200,
                headers: BTreeMap::new(),
                body: fixture_body,
            })
        })
    });

    let resolver: maho_omo_x_search::tool::BearerResolver =
        Arc::new(|_reg, _env| Box::pin(async { Some(("injected".to_string(), "env")) }));

    let (_def, exec) = create_x_search_tool(CreateXSearchToolOptions {
        fetch_impl: Some(fetch_impl),
        env: Some(BTreeMap::new()),
        resolve_bearer: Some(resolver),
    });

    let ctx = make_context(None, None);

    let _result = exec("call-1", json!({ "query": "Grok CLI" }), None, None, &ctx)
        .await
        .expect("execution succeeds");

    let auth_header = captured_auth.lock().unwrap();
    assert_eq!(*auth_header, "Bearer injected");
}

#[tokio::test]
async fn test_execute_caller_abort_returns_upstream_error() {
    let controller = maho_ai::utils::abort::AbortController::new();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let started_tx = Arc::new(std::sync::Mutex::new(Some(started_tx)));

    let fetch_impl: XSearchFetch = Arc::new(move |_url, _headers, _body| {
        let started_tx = started_tx.clone();
        Box::pin(async move {
            if let Some(tx) = started_tx.lock().unwrap().take() {
                let _ = tx.send(());
            }
            std::future::pending::<()>().await;
            Ok(XSearchHttpResponse {
                status: 200,
                headers: BTreeMap::new(),
                body: "{}".to_string(),
            })
        })
    });

    let (_def, exec) = create_x_search_tool(CreateXSearchToolOptions {
        fetch_impl: Some(fetch_impl),
        env: Some(BTreeMap::new()),
        resolve_bearer: None,
    });

    let ctx = make_context(
        Some(maho_ai::auth::types::CredentialType::OAuth),
        Some("stored-token"),
    );

    let controller_clone = controller.clone();
    let driver = tokio::spawn(async move {
        started_rx.await.expect("fetch started");
        controller_clone.abort(None);
    });

    let result = exec(
        "call-1",
        json!({ "query": "Grok CLI" }),
        Some(controller.signal()),
        None,
        &ctx,
    )
    .await
    .expect("execution succeeds");

    driver.await.expect("driver join");

    let text = text_of(&result);
    assert!(text.contains("x_search error [UPSTREAM]"));
    assert!(text.contains("cancelled by the caller"));
    assert_eq!(result.is_error, Some(true));
}

#[test]
fn test_tool_definition_parameters_schema() {
    let (def, _exec) = create_x_search_tool(CreateXSearchToolOptions::default());
    let schema = def.parameters.as_object().expect("schema object");
    assert_eq!(schema.get("type").and_then(Value::as_str), Some("object"));
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .expect("properties");
    assert!(properties.contains_key("query"));
    assert!(properties.contains_key("allowed_x_handles"));
    assert!(properties.contains_key("excluded_x_handles"));
    assert!(properties.contains_key("from_date"));
    assert!(properties.contains_key("to_date"));
    assert!(properties.contains_key("mode"));
    assert!(properties.contains_key("max_results"));
}
