//! Transport seam for the OAuth flows.
//!
//! The TS flows call the global `fetch`, which senpi's tests replace with `vi.stubGlobal`.
//! Rust has no replaceable global, so each flow takes an [`OAuthTransport`]; the default
//! implementation is reqwest against the real network, and tests drive a scripted one.

use crate::utils::abort::{AbortReason, AbortSignal};
use async_trait::async_trait;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl HttpResponse {
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(key, _)| key.eq_ignore_ascii_case(name)).map(|(_, value)| value.as_str())
    }

    pub fn json(&self) -> anyhow::Result<serde_json::Value> {
        Ok(serde_json::from_str(&self.body)?)
    }
}

#[async_trait]
pub trait OAuthTransport: Send + Sync {
    async fn execute(&self, request: HttpRequest, signal: &AbortSignal) -> anyhow::Result<HttpResponse>;

    fn now_ms(&self) -> f64 {
        system_now_ms()
    }
}

pub fn system_now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as f64)
        .unwrap_or(0.0)
}

pub struct ReqwestTransport {
    client: reqwest::Client,
}

impl Default for ReqwestTransport {
    fn default() -> Self {
        Self { client: reqwest::Client::new() }
    }
}

impl ReqwestTransport {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl OAuthTransport for ReqwestTransport {
    async fn execute(&self, request: HttpRequest, signal: &AbortSignal) -> anyhow::Result<HttpResponse> {
        if let Err(reason) = signal.throw_if_aborted() {
            anyhow::bail!("{reason}");
        }
        let method = reqwest::Method::from_bytes(request.method.as_bytes())?;
        let mut builder = self.client.request(method, &request.url);
        for (key, value) in &request.headers {
            builder = builder.header(key, value);
        }
        if let Some(body) = &request.body {
            builder = builder.body(body.clone());
        }
        if let Some(timeout_ms) = request.timeout_ms {
            builder = builder.timeout(Duration::from_millis(timeout_ms));
        }
        let response = tokio::select! {
            biased;
            () = signal.cancelled() => anyhow::bail!("{}", signal.reason().unwrap_or_else(AbortReason::dom_default)),
            result = builder.send() => result?,
        };
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(key, value)| (key.as_str().to_string(), value.to_str().unwrap_or_default().to_string()))
            .collect();
        let body = response.text().await?;
        Ok(HttpResponse { status, body, headers })
    }
}

pub fn default_transport() -> Arc<dyn OAuthTransport> {
    Arc::new(ReqwestTransport::new())
}

#[derive(Debug, Clone)]
pub enum ScriptedResponse {
    Json { status: u16, body: serde_json::Value },
    Text { status: u16, body: String },
    Failure { message: String },
}

#[derive(Default)]
pub struct ScriptedTransport {
    responses: Mutex<Vec<(Option<String>, ScriptedResponse)>>,
    requests: Mutex<Vec<HttpRequest>>,
    now_ms: Mutex<Option<f64>>,
}

impl ScriptedTransport {
    pub fn new(responses: Vec<(Option<&str>, ScriptedResponse)>) -> Self {
        Self {
            responses: Mutex::new(
                responses.into_iter().map(|(matcher, response)| (matcher.map(str::to_string), response)).collect(),
            ),
            requests: Mutex::new(Vec::new()),
            now_ms: Mutex::new(None),
        }
    }

    pub fn set_now_ms(&self, now_ms: f64) {
        *self.now_ms.lock().unwrap_or_else(|p| p.into_inner()) = Some(now_ms);
    }

    pub fn advance_ms(&self, delta_ms: f64) {
        let mut now_ms = self.now_ms.lock().unwrap_or_else(|p| p.into_inner());
        let base = now_ms.unwrap_or_else(system_now_ms);
        *now_ms = Some(base + delta_ms);
    }

    pub fn requests(&self) -> Vec<HttpRequest> {
        self.requests.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    pub fn push(&self, matcher: Option<&str>, response: ScriptedResponse) {
        self.responses
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push((matcher.map(str::to_string), response));
    }
}

#[async_trait]
impl OAuthTransport for ScriptedTransport {
    fn now_ms(&self) -> f64 {
        self.now_ms.lock().unwrap_or_else(|p| p.into_inner()).unwrap_or_else(system_now_ms)
    }

    async fn execute(&self, request: HttpRequest, _signal: &AbortSignal) -> anyhow::Result<HttpResponse> {
        self.requests.lock().unwrap_or_else(|p| p.into_inner()).push(request.clone());
        let index = {
            let responses = self.responses.lock().unwrap_or_else(|p| p.into_inner());
            responses
                .iter()
                .position(|(matcher, _)| matcher.as_ref().is_none_or(|matcher| request.url.contains(matcher.as_str())))
        };
        let Some(index) = index else {
            anyhow::bail!("ScriptedTransport has no response for {}", request.url);
        };
        let response = self.responses.lock().unwrap_or_else(|p| p.into_inner()).remove(index).1;
        match response {
            ScriptedResponse::Json { status, body } => Ok(HttpResponse {
                status,
                headers: vec![("content-type".into(), "application/json".into())],
                body: body.to_string(),
            }),
            ScriptedResponse::Text { status, body } => Ok(HttpResponse { status, headers: Vec::new(), body }),
            ScriptedResponse::Failure { message } => anyhow::bail!("{message}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::abort::AbortController;

    fn request(url: &str) -> HttpRequest {
        HttpRequest {
            method: "POST".into(),
            url: url.into(),
            headers: Vec::new(),
            body: None,
            timeout_ms: None,
        }
    }

    #[tokio::test]
    async fn scripted_transport_matches_by_url_substring_and_records_requests() {
        let transport = ScriptedTransport::new(vec![
            (Some("/oauth/token"), ScriptedResponse::Json { status: 200, body: serde_json::json!({"a": 1}) }),
            (None, ScriptedResponse::Text { status: 404, body: "missing".into() }),
        ]);
        let signal = AbortController::new().signal();

        let matched = transport.execute(request("https://example.com/oauth/token"), &signal).await.unwrap();
        assert!(matched.ok());
        assert_eq!(matched.json().unwrap()["a"], 1);

        let fallback = transport.execute(request("https://example.com/other"), &signal).await.unwrap();
        assert_eq!(fallback.status, 404);
        assert_eq!(transport.requests().len(), 2);
    }

    #[tokio::test]
    async fn scripted_transport_reports_missing_responses() {
        let transport = ScriptedTransport::new(Vec::new());
        let signal = AbortController::new().signal();
        let error = transport.execute(request("https://example.com/x"), &signal).await.unwrap_err();
        assert!(error.to_string().contains("no response for https://example.com/x"));
    }
}
