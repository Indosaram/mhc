//! Request construction (spec §4.3-4.4) and the HTTP seam.

use crate::params::Params;
use serde_json::{Map, Value, json};
use std::future::Future;
use std::pin::Pin;

/// xAI Responses endpoint (interface fact).
pub const ENDPOINT: &str = "https://api.x.ai/v1/responses";
/// Default model (interface fact).
pub const DEFAULT_MODEL: &str = "grok-4.20-0309-non-reasoning";
/// Model override kept for compatibility with existing setups.
pub const MODEL_ENV: &str = "OMO_X_SEARCH_MODEL";
/// Neutral model override; wins over [`MODEL_ENV`].
pub const NEUTRAL_MODEL_ENV: &str = "MAHO_X_SEARCH_MODEL";
pub const MAX_OUTPUT_TOKENS: u64 = 4000;

/// The model for this call: the neutral override, else the compatibility override, else the default.
pub fn choose_model(env: &dyn Fn(&str) -> Option<String>) -> String {
    [NEUTRAL_MODEL_ENV, MODEL_ENV]
        .iter()
        .find_map(|key| {
            env(key)
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
        })
        .unwrap_or_else(|| DEFAULT_MODEL.to_owned())
}

/// The user message: the query (with `since:`/`until:` operators) plus mode and result budget.
pub fn prompt(params: &Params) -> String {
    let mut query = params.query.clone();
    if let Some(from) = &params.from_date {
        query.push_str(&format!(" since:{from}"));
    }
    if let Some(to) = &params.to_date {
        query.push_str(&format!(" until:{to}"));
    }
    format!(
        "Use X search to find posts for this query: {query}\n\
         Ordering: {mode} posts first.\n\
         Return up to {max} matching posts. For each post give its x.com status URL and one short line on what it says. Do not invent posts or URLs.",
        mode = params.mode.as_str(),
        max = params.max_results,
    )
}

/// The full JSON body for `POST /v1/responses`.
pub fn body(params: &Params, model: &str) -> Value {
    let mut tool = Map::new();
    tool.insert("type".into(), json!("x_search"));
    if let Some(from) = &params.from_date {
        tool.insert("from_date".into(), json!(from));
    }
    if let Some(to) = &params.to_date {
        tool.insert("to_date".into(), json!(to));
    }
    if let Some(handles) = &params.allowed_x_handles {
        tool.insert("allowed_x_handles".into(), json!(handles));
    }
    if let Some(handles) = &params.excluded_x_handles {
        tool.insert("excluded_x_handles".into(), json!(handles));
    }
    tool.insert(
        "enable_image_understanding".into(),
        json!(params.enable_image_understanding),
    );
    tool.insert(
        "enable_video_understanding".into(),
        json!(params.enable_video_understanding),
    );
    json!({
        "model": model,
        "input": [{"role": "user", "content": prompt(params)}],
        "tools": [Value::Object(tool)],
        "tool_choice": "required",
        "max_turns": 1,
        "parallel_tool_calls": false,
        "max_output_tokens": MAX_OUTPUT_TOKENS,
        "store": false,
    })
}

/// One outgoing request.
#[derive(Clone, Debug, PartialEq)]
pub struct HttpRequest {
    pub url: String,
    /// Lower-case header names.
    pub headers: Vec<(String, String)>,
    pub body: Value,
}

/// One received response.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    /// Numeric `retry-after` seconds, when the server sent one.
    pub retry_after: Option<u64>,
    pub body: String,
}

pub type HttpFuture<'a> = Pin<Box<dyn Future<Output = Result<HttpResponse, String>> + Send + 'a>>;

/// The HTTP transport; `Err` is a transport failure message.
pub trait HttpClient: Send + Sync {
    fn post(&self, request: HttpRequest) -> HttpFuture<'_>;
}

/// Parses a numeric `retry-after` header value (seconds).
pub fn parse_retry_after(value: Option<&str>) -> Option<u64> {
    value.and_then(|value| value.trim().parse().ok())
}

/// The production transport over reqwest (rustls).
#[derive(Clone, Debug, Default)]
pub struct ReqwestClient {
    client: reqwest::Client,
}

impl HttpClient for ReqwestClient {
    fn post(&self, request: HttpRequest) -> HttpFuture<'_> {
        Box::pin(async move {
            let mut builder = self.client.post(&request.url);
            for (name, value) in &request.headers {
                builder = builder.header(name.as_str(), value.as_str());
            }
            let response = builder
                .body(request.body.to_string())
                .send()
                .await
                .map_err(|error| error.to_string())?;
            let status = response.status().as_u16();
            let retry_after = parse_retry_after(
                response
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|value| value.to_str().ok()),
            );
            let body = response.text().await.map_err(|error| error.to_string())?;
            Ok(HttpResponse {
                status,
                retry_after,
                body,
            })
        })
    }
}
