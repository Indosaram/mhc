//! Port of omo-senpi `components/x-search/client.ts` at latest `455dee62`.

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use maho_ai::utils::abort::AbortSignal;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::format::{NormalizedXSearch, XSearchErrorCode, XSearchResultItem, XSearchUsage};
use crate::params::XSearchParams;

pub const X_SEARCH_ENDPOINT: &str = "https://api.x.ai/v1/responses";
pub const X_SEARCH_MAX_OUTPUT_TOKENS: u64 = 4000;
pub const X_SEARCH_DEFAULT_DEADLINE_MS: u64 = 60_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XSearchRequestParams {
    pub query: String,
    pub mode: String,
    pub max_results: usize,
    pub from_date: Option<String>,
    pub to_date: Option<String>,
    pub allowed_x_handles: Option<Vec<String>>,
    pub excluded_x_handles: Option<Vec<String>>,
    pub enable_image_understanding: Option<bool>,
    pub enable_video_understanding: Option<bool>,
}

impl From<&XSearchParams> for XSearchRequestParams {
    fn from(p: &XSearchParams) -> Self {
        Self {
            query: p.query.clone(),
            mode: p.mode.clone(),
            max_results: p.max_results,
            from_date: p.from_date.clone(),
            to_date: p.to_date.clone(),
            allowed_x_handles: p.allowed_x_handles.clone(),
            excluded_x_handles: p.excluded_x_handles.clone(),
            enable_image_understanding: Some(p.enable_image_understanding),
            enable_video_understanding: Some(p.enable_video_understanding),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct XSearchCarrier {
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ReasoningEffort>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReasoningEffort {
    pub effort: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XSearchPromptVariant {
    V1,
    V2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XSearchCarrierName {
    Fast,
    Reasoning,
}

pub fn carrier_model(name: XSearchCarrierName) -> XSearchCarrier {
    match name {
        XSearchCarrierName::Fast => XSearchCarrier {
            model: "grok-4.20-0309-non-reasoning".to_string(),
            reasoning: None,
        },
        XSearchCarrierName::Reasoning => XSearchCarrier {
            model: "grok-4.6".to_string(),
            reasoning: Some(ReasoningEffort { effort: "low".to_string() }),
        },
    }
}

pub const DEFAULT_PROMPT_VARIANT: XSearchPromptVariant = XSearchPromptVariant::V1;
pub const DEFAULT_CARRIER: XSearchCarrierName = XSearchCarrierName::Fast;

pub fn prompt_v1(params: &XSearchRequestParams) -> String {
    let since = params.from_date.as_ref().map_or(String::new(), |d| format!(" since:{d}"));
    let until = params.to_date.as_ref().map_or(String::new(), |d| format!(" until:{d}"));
    let mode_str = if params.mode == "top" { "Top" } else { "Latest" };
    format!(
        "Use the x_search tool with x_keyword_search only, mode={}. Run exactly this query: \"{}{}{}\". Return up to {} X posts, one per line as \"URL - one-line summary (@handle, YYYY-MM-DD)\". Posts only, no commentary.",
        mode_str, params.query, since, until, params.max_results
    )
}

pub fn prompt_v2(params: &XSearchRequestParams) -> String {
    format!(
        "{} Prefer the most recent posts; keep the exact since: operator inside the keyword query.",
        prompt_v1(params)
    )
}

pub fn render_prompt(variant: XSearchPromptVariant, params: &XSearchRequestParams) -> String {
    match variant {
        XSearchPromptVariant::V1 => prompt_v1(params),
        XSearchPromptVariant::V2 => prompt_v2(params),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct XSearchRequestBody {
    pub model: String,
    pub input: Vec<XSearchInputMessage>,
    pub tools: Vec<Value>,
    pub tool_choice: String,
    pub max_turns: u32,
    pub parallel_tool_calls: bool,
    pub max_output_tokens: u64,
    pub store: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ReasoningEffort>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct XSearchInputMessage {
    pub role: String,
    pub content: String,
}

pub fn build_x_search_request(
    params: &XSearchRequestParams,
    carrier: &XSearchCarrier,
    variant: XSearchPromptVariant,
) -> XSearchRequestBody {
    let mut tool_obj = serde_json::Map::new();
    tool_obj.insert("type".to_string(), json!("x_search"));
    if let Some(ref from) = params.from_date {
        tool_obj.insert("from_date".to_string(), json!(from));
    }
    if let Some(ref to) = params.to_date {
        tool_obj.insert("to_date".to_string(), json!(to));
    }
    if let Some(ref allowed) = params.allowed_x_handles {
        tool_obj.insert("allowed_x_handles".to_string(), json!(allowed));
    }
    if let Some(ref excluded) = params.excluded_x_handles {
        tool_obj.insert("excluded_x_handles".to_string(), json!(excluded));
    }
    if let Some(img) = params.enable_image_understanding {
        tool_obj.insert("enable_image_understanding".to_string(), json!(img));
    }
    if let Some(vid) = params.enable_video_understanding {
        tool_obj.insert("enable_video_understanding".to_string(), json!(vid));
    }

    XSearchRequestBody {
        model: carrier.model.clone(),
        input: vec![XSearchInputMessage {
            role: "user".to_string(),
            content: render_prompt(variant, params),
        }],
        tools: vec![Value::Object(tool_obj)],
        tool_choice: "required".to_string(),
        max_turns: 1,
        parallel_tool_calls: false,
        max_output_tokens: X_SEARCH_MAX_OUTPUT_TOKENS,
        store: false,
        reasoning: carrier.reasoning.clone(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum XSearchFetchOutcome {
    Ok(Value),
    Err {
        code: XSearchErrorCode,
        status: Option<u16>,
        retry_after: Option<u64>,
        message: String,
    },
}

#[derive(Clone, Debug)]
pub struct XSearchHttpResponse {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: String,
}

pub type XSearchFetch = Arc<
    dyn Fn(
            String,
            BTreeMap<String, String>,
            String,
        ) -> Pin<Box<dyn Future<Output = Result<XSearchHttpResponse, String>> + Send>>
        + Send
        + Sync,
>;

pub fn default_fetch() -> XSearchFetch {
    Arc::new(|url, headers, body| {
        Box::pin(async move {
            let client = reqwest::Client::new();
            let mut req = client.post(&url);
            for (k, v) in headers {
                req = req.header(k, v);
            }
            let resp = req.body(body).send().await.map_err(|e| e.to_string())?;
            let status = resp.status().as_u16();
            let mut out_headers = BTreeMap::new();
            for (k, v) in resp.headers() {
                if let Ok(v_str) = v.to_str() {
                    out_headers.insert(k.as_str().to_ascii_lowercase(), v_str.to_string());
                }
            }
            let text = resp.text().await.map_err(|e| e.to_string())?;
            Ok(XSearchHttpResponse {
                status,
                headers: out_headers,
                body: text,
            })
        })
    })
}

pub struct PerformXSearchOptions {
    pub fetch: XSearchFetch,
    pub bearer: String,
    pub body: Value,
    pub signal: Option<AbortSignal>,
    pub deadline_ms: Option<u64>,
    pub endpoint: Option<String>,
}

pub async fn perform_x_search(options: PerformXSearchOptions) -> XSearchFetchOutcome {
    let deadline_ms = options.deadline_ms.unwrap_or(X_SEARCH_DEFAULT_DEADLINE_MS);
    let endpoint = options.endpoint.unwrap_or_else(|| X_SEARCH_ENDPOINT.to_string());

    // Check if caller signal is already aborted before starting
    if let Some(ref signal) = options.signal
        && signal.aborted()
    {
        return XSearchFetchOutcome::Err {
            code: XSearchErrorCode::Upstream,
            status: None,
            retry_after: None,
            message: "xAI request was cancelled by the caller".to_string(),
        };
    }

    let mut headers = BTreeMap::new();
    headers.insert("content-type".to_string(), "application/json".to_string());
    headers.insert("authorization".to_string(), format!("Bearer {}", options.bearer));

    let body_str = serde_json::to_string(&options.body).unwrap_or_default();

    let fetch_fut = (options.fetch)(endpoint, headers, body_str);

    let sleep_fut = tokio::time::sleep(Duration::from_millis(deadline_ms));
    tokio::pin!(sleep_fut);

    let caller_signal = options.signal;

    let response_res: Result<XSearchHttpResponse, String>;

    tokio::select! {
        biased;

        _ = async {
            if let Some(ref sig) = caller_signal {
                sig.cancelled().await;
            } else {
                futures::future::pending::<()>().await;
            }
        } => {
            return XSearchFetchOutcome::Err {
                code: XSearchErrorCode::Upstream,
                status: None,
                retry_after: None,
                message: "xAI request was cancelled by the caller".to_string(),
            };
        }

        _ = &mut sleep_fut => {
            return XSearchFetchOutcome::Err {
                code: XSearchErrorCode::Timeout,
                status: None,
                retry_after: None,
                message: format!("xAI request aborted after {deadline_ms}ms"),
            };
        }

        res = fetch_fut => {
            response_res = res;
        }
    }

    let response = match response_res {
        Ok(resp) => resp,
        Err(err) => {
            return XSearchFetchOutcome::Err {
                code: XSearchErrorCode::Upstream,
                status: None,
                retry_after: None,
                message: err,
            };
        }
    };

    let status = response.status;
    if status == 401 || status == 403 {
        return XSearchFetchOutcome::Err {
            code: XSearchErrorCode::Auth,
            status: Some(status),
            retry_after: None,
            message: "xAI rejected the credential".to_string(),
        };
    }
    if status == 429 {
        let retry_after = response
            .headers
            .get("retry-after")
            .and_then(|val| val.trim().parse::<u64>().ok());
        return XSearchFetchOutcome::Err {
            code: XSearchErrorCode::RateLimited,
            status: Some(status),
            retry_after,
            message: "xAI rate limited the request".to_string(),
        };
    }
    if !(200..=299).contains(&status) {
        return XSearchFetchOutcome::Err {
            code: XSearchErrorCode::Upstream,
            status: Some(status),
            retry_after: None,
            message: format!("xAI responded with HTTP {status}"),
        };
    }

    match serde_json::from_str::<Value>(&response.body) {
        Ok(parsed) => XSearchFetchOutcome::Ok(parsed),
        Err(_) => XSearchFetchOutcome::Err {
            code: XSearchErrorCode::Protocol,
            status: Some(status),
            retry_after: None,
            message: "xAI returned a non-JSON response".to_string(),
        },
    }
}

struct MessageText {
    text: String,
    annotations: Vec<Value>,
}

fn collect_message_texts(output: &[Value]) -> Vec<MessageText> {
    let mut result = Vec::new();
    for raw_item in output {
        let Some(item) = raw_item.as_object() else { continue };
        if item.get("type").and_then(Value::as_str) != Some("message") {
            continue;
        }
        let Some(content_arr) = item.get("content").and_then(Value::as_array) else { continue };
        for raw_part in content_arr {
            let Some(part) = raw_part.as_object() else { continue };
            let Some(text) = part.get("text").and_then(Value::as_str) else { continue };
            if text.is_empty() {
                continue;
            }
            let annotations = part
                .get("annotations")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            result.push(MessageText {
                text: text.to_string(),
                annotations,
            });
        }
    }
    result
}

fn tweet_id(url: &str) -> Option<String> {
    let re = Regex::new(r"https?://(?:www.)?x.com/(?:[A-Za-z0-9_]+/status|i/status)/([0-9]{1,30})").ok()?;
    re.captures(url).and_then(|cap| cap.get(1)).map(|m| m.as_str().to_string())
}

fn descriptive_title<'a>(raw_title: Option<&'a str>, url: &'a str) -> &'a str {
    let Some(title) = raw_title else { return url };
    if title.is_empty() { return url; }
    if title.chars().all(|c| c.is_ascii_digit()) { return url; }
    if title.starts_with("http://") || title.starts_with("https://") { return url; }
    title
}

fn snippet_for(url: &str, id: &str, texts: &[MessageText]) -> String {
    let id_status = format!("/status/{id}");
    for entry in texts {
        for line in entry.text.split('\n') {
            if line.contains(url) || line.contains(&id_status) {
                return line.trim().to_string();
            }
        }
    }
    String::new()
}

fn collect_queries(output: &[Value]) -> Vec<String> {
    const SERVER_QUERY_TOOL_NAMES: &[&str] = &[
        "x_keyword_search",
        "x_semantic_search",
        "x_user_search",
        "x_thread_fetch",
    ];
    let mut queries = Vec::new();
    for raw_item in output {
        let Some(item) = raw_item.as_object() else { continue };
        let item_type = item.get("type").and_then(Value::as_str);
        if item_type != Some("custom_tool_call") && item_type != Some("x_search_call") {
            continue;
        }
        let Some(name) = item.get("name").and_then(Value::as_str) else { continue };
        if !SERVER_QUERY_TOOL_NAMES.contains(&name) {
            continue;
        }
        let Some(input_str) = item.get("input").and_then(Value::as_str) else { continue };
        let Ok(parsed) = serde_json::from_str::<Value>(input_str) else { continue };
        let Some(obj) = parsed.as_object() else { continue };
        let parts: Vec<String> = obj
            .iter()
            .map(|(k, v)| {
                let v_str = match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                format!("{k}={v_str}")
            })
            .collect();
        queries.push(parts.join(" "));
    }
    queries
}

pub fn normalize_x_search_response(raw: &Value, max_results: Option<usize>) -> NormalizedXSearch {
    let payload = raw.as_object();
    let empty_arr = Vec::new();
    let output = payload
        .and_then(|p| p.get("output"))
        .and_then(Value::as_array)
        .unwrap_or(&empty_arr);

    let texts = collect_message_texts(output);

    let mut by_id = HashMap::new();
    let mut order = Vec::new();

    let mut add = |url: String, title: Option<String>| {
        let Some(id) = tweet_id(&url) else { return };
        if by_id.contains_key(&id) {
            return;
        }
        let title_resolved = descriptive_title(title.as_deref(), &url).to_string();
        let snippet = snippet_for(&url, &id, &texts);
        let item = XSearchResultItem {
            id: id.clone(),
            url,
            title: title_resolved,
            snippet,
        };
        by_id.insert(id.clone(), item);
        order.push(id);
    };

    // 1. URL citations from annotations
    for entry in &texts {
        for raw_ann in &entry.annotations {
            let Some(ann) = raw_ann.as_object() else { continue };
            if ann.get("type").and_then(Value::as_str) != Some("url_citation") {
                continue;
            }
            let Some(url) = ann.get("url").and_then(Value::as_str) else { continue };
            let title = ann.get("title").and_then(Value::as_str).map(str::to_string);
            add(url.to_string(), title);
        }
    }

    // 2. Tweet URLs matched in message text
    if let Ok(tweet_url_re) = Regex::new(r"https?://(?:www.)?x.com/(?:[A-Za-z0-9_]+/status|i/status)/[0-9]{1,30}") {
        for entry in &texts {
            for m in tweet_url_re.find_iter(&entry.text) {
                add(m.as_str().to_string(), None);
            }
        }
    }

    // 3. Top-level citations
    if let Some(citations) = payload.and_then(|p| p.get("citations")).and_then(Value::as_array) {
        for c in citations {
            if let Some(url) = c.as_str() {
                add(url.to_string(), None);
            }
        }
    }

    let mut results: Vec<XSearchResultItem> = order.into_iter().filter_map(|id| by_id.remove(&id)).collect();
    if let Some(max) = max_results {
        results.truncate(max);
    }

    let queries = collect_queries(output);

    let usage_obj = payload.and_then(|p| p.get("usage")).and_then(Value::as_object);
    let tool_usage_obj = usage_obj
        .and_then(|u| u.get("server_side_tool_usage_details"))
        .and_then(Value::as_object);
    let x_search_calls = tool_usage_obj
        .and_then(|tu| tu.get("x_search_calls"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let cost_ticks = usage_obj
        .and_then(|u| u.get("cost_in_usd_ticks"))
        .and_then(Value::as_u64)
        .unwrap_or(0);

    NormalizedXSearch {
        results,
        queries,
        usage: XSearchUsage {
            x_search_calls,
            cost_ticks,
        },
    }
}
