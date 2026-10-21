//! Port of omo-senpi `components/x-search/tool.ts` at latest `455dee62`.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use maho_ext_api::{
    AgentToolResult, ContentBlock, ExtensionContext, ExtensionFailure, ExtensionToolExecutor,
    JsonValue, ModelRegistry, ToolDefinition, ToolExecutionMode, ToolExposure,
};
use serde::{Deserialize, Serialize};

use crate::auth::resolve_xai_bearer;
use crate::client::{
    build_x_search_request, carrier_model, default_fetch, normalize_x_search_response,
    perform_x_search, PerformXSearchOptions, XSearchFetch, XSearchRequestParams,
    DEFAULT_CARRIER, DEFAULT_PROMPT_VARIANT,
};
use crate::format::{
    format_x_search_error, format_x_search_result, XSearchErrorCode, XSearchResultItem,
    XSearchUsage,
};
use crate::params::{
    validate_x_search_params, x_search_parameters_schema, XSearchValidation,
};

pub const X_SEARCH_TOOL_NAME: &str = "x_search";
pub const X_SEARCH_MODEL_ENV: &str = "OMO_X_SEARCH_MODEL";

pub const X_SEARCH_TOOL_DESCRIPTION: &str =
    "Searches X (Twitter) posts through xAI.     Date-bound time-sensitive queries with from_date (>= yesterday), scope trusted accounts with allowed_x_handles (max 20), phrase as latest/recent, and run 2-3 narrower searches (by handle, by keyword) instead of one broad query.     Returns x.com URLs with one-line summaries plus the exact server queries used.     Requires a connected xAI account.";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct XSearchToolDetails {
    pub results: Vec<XSearchResultItem>,
    pub queries: Vec<String>,
    pub usage: XSearchUsage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<XSearchErrorCode>,
}

pub type BearerResolver = Arc<
    dyn for<'a> Fn(
            &'a (dyn ModelRegistry + 'a),
            &'a BTreeMap<String, String>,
        ) -> Pin<Box<dyn Future<Output = Option<(String, &'static str)>> + Send + 'a>>
        + Send
        + Sync,
>;

#[derive(Default)]
pub struct CreateXSearchToolOptions {
    pub fetch_impl: Option<XSearchFetch>,
    pub env: Option<BTreeMap<String, String>>,
    pub resolve_bearer: Option<BearerResolver>,
}

fn error_result(code: XSearchErrorCode, message: &str) -> AgentToolResult {
    let text = format_x_search_error(code, message);
    let details = XSearchToolDetails {
        results: Vec::new(),
        queries: Vec::new(),
        usage: XSearchUsage::default(),
        code: Some(code),
    };
    AgentToolResult {
        content: vec![ContentBlock::text(text)],
        details: serde_json::to_value(details).unwrap_or(JsonValue::Null),
        structured_content: None,
        usage: None,
        nested_calls: None,
        added_tool_names: None,
        terminate: None,
        is_error: Some(true),
    }
}

pub fn create_x_search_tool(
    options: CreateXSearchToolOptions,
) -> (ToolDefinition, ExtensionToolExecutor) {
    let env_captured = options.env;
    let fetch_impl = options.fetch_impl.unwrap_or_else(default_fetch);
    let resolve_bearer_impl = options.resolve_bearer;

    let mut definition = ToolDefinition::new(
        X_SEARCH_TOOL_NAME,
        X_SEARCH_TOOL_DESCRIPTION,
        x_search_parameters_schema(),
        Arc::new(|_| Box::pin(async { panic!("legacy executor must not run") })),
    );
    definition.label = "Search X posts".to_string();
    definition.exposure = Some(ToolExposure::Search);
    definition.search_group = Some("x-search".to_string());
    definition.search_keywords = Some(vec![
        "X posts".to_string(),
        "tweets".to_string(),
        "twitter search".to_string(),
        "xAI live search".to_string(),
        "what people are saying on X".to_string(),
    ]);
    definition.allow_lazy_activation = Some(true);
    definition.execution_mode = Some(ToolExecutionMode::Parallel);

    let executor: ExtensionToolExecutor = Arc::new(move |_id, params, signal, _update, context: &ExtensionContext| {
        let env = env_captured.clone().unwrap_or_else(|| std::env::vars().collect());
        let fetch = fetch_impl.clone();
        let resolve_bearer_fn = resolve_bearer_impl.clone();
        let model_registry = context.model_registry.clone();
        let signal_opt = signal;

        Box::pin(async move {
            let validation = validate_x_search_params(&params);
            let valid_params = match validation {
                XSearchValidation::Ok(p) => p,
                XSearchValidation::Err { code, message } => {
                    return Ok(error_result(code, &message));
                }
            };

            let credential = match resolve_bearer_fn {
                Some(ref resolver) => resolver(model_registry.as_ref(), &env).await,
                None => resolve_xai_bearer(model_registry.as_ref(), &env).await,
            };

            let Some((bearer, _provenance)) = credential else {
                return Ok(error_result(
                    XSearchErrorCode::Auth,
                    "no xAI credential is connected; run /login and pick xAI, or set XAI_API_KEY",
                ));
            };

            let mut carrier = carrier_model(DEFAULT_CARRIER);
            if let Some(override_model) = env.get(X_SEARCH_MODEL_ENV).map(String::as_str).map(str::trim).filter(|s| !s.is_empty()) {
                carrier.model = override_model.to_string();
            }

            let req_params = XSearchRequestParams::from(&valid_params);
            let body = build_x_search_request(&req_params, &carrier, DEFAULT_PROMPT_VARIANT);
            let body_val = serde_json::to_value(&body).map_err(|e| ExtensionFailure::new(e.to_string()))?;

            let outcome = perform_x_search(PerformXSearchOptions {
                fetch,
                bearer,
                body: body_val,
                signal: signal_opt,
                deadline_ms: None,
                endpoint: None,
            }).await;

            match outcome {
                crate::client::XSearchFetchOutcome::Err { code, message, .. } => {
                    Ok(error_result(code, &message))
                }
                crate::client::XSearchFetchOutcome::Ok(raw) => {
                    let normalized = normalize_x_search_response(&raw, Some(valid_params.max_results));
                    let text = format_x_search_result(&normalized);
                    let details = XSearchToolDetails {
                        results: normalized.results,
                        queries: normalized.queries,
                        usage: normalized.usage,
                        code: None,
                    };
                    Ok(AgentToolResult {
                        content: vec![ContentBlock::text(text)],
                        details: serde_json::to_value(details).unwrap_or(JsonValue::Null),
                        structured_content: None,
                        usage: None,
                        nested_calls: None,
                        added_tool_names: None,
                        terminate: None,
                        is_error: None,
                    })
                }
            }
        })
    });

    (definition, executor)
}
