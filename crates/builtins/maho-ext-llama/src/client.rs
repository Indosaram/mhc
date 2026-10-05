//! Port of `packages/coding-agent/src/extensions/llama/client.ts` (pin `fe8c564b`).

use std::collections::BTreeMap;
use std::time::Duration;

use maho_ai::utils::abort::AbortSignal;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

pub const REQUEST_TIMEOUT_MS: u64 = 15_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LlamaModelStatus {
    Unloaded,
    Loading,
    Loaded,
    Downloading,
    Sleeping,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LlamaModelState {
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LlamaModelArchitecture {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_modalities: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_modalities: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LlamaModelMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub n_ctx: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub n_ctx_train: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ftype: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LlamaModelInfo {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aliases: Option<Vec<String>>,
    pub status: LlamaModelState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub architecture: Option<LlamaModelArchitecture>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<LlamaModelMeta>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct LlamaServerProps {
    pub models_autoload: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlamaModelEvent {
    pub model: String,
    pub event: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct LlamaProgress {
    pub message: String,
    pub ratio: Option<f64>,
    pub detail: Option<String>,
}

fn error_message(payload: &Value, fallback: &str) -> String {
    let Some(message) = payload.get("error").and_then(|error| error.get("message")).and_then(Value::as_str) else {
        return fallback.to_owned();
    };
    if message.is_empty() { fallback.to_owned() } else { message.to_owned() }
}

fn is_model_info(value: &Value) -> bool {
    value.get("id").and_then(Value::as_str).is_some()
        && value.get("status").and_then(|status| status.get("value")).and_then(Value::as_str).is_some()
}

fn parse_load_progress(data: &Value) -> Option<LlamaProgress> {
    let progress = data.get("progress")?;
    let stage = progress
        .get("current")
        .and_then(Value::as_str)
        .or_else(|| progress.get("stage").and_then(Value::as_str));
    let stages: Vec<String> = progress
        .get("stages")
        .and_then(Value::as_array)
        .map(|stages| stages.iter().filter_map(|stage| stage.as_str().map(str::to_owned)).collect())
        .unwrap_or_default();
    let stage_ratio = progress.get("value").and_then(Value::as_f64).map(|value| value.clamp(0.0, 1.0));
    let mut ratio = stage_ratio;
    if let Some(stage) = stage
        && !stages.is_empty()
        && let Some(index) = stages.iter().position(|candidate| candidate == stage)
    {
        ratio = Some((index as f64 + stage_ratio.unwrap_or(0.0)) / stages.len() as f64);
    }
    Some(LlamaProgress {
        message: stage.map_or_else(|| "Loading model".to_owned(), |stage| format!("Loading {}", stage.replace('_', " "))),
        ratio,
        detail: None,
    })
}

fn parse_download_progress(data: &Value) -> Option<LlamaProgress> {
    let files = data.get("progress").filter(|value| value.is_object()).unwrap_or(data);
    let (mut done, mut total) = (0.0_f64, 0.0_f64);
    if let Some(object) = files.as_object() {
        for value in object.values() {
            let (Some(done_value), Some(total_value)) = (value.get("done").and_then(Value::as_f64), value.get("total").and_then(Value::as_f64)) else { continue; };
            done += done_value;
            total += total_value;
        }
    }
    if total <= 0.0 { return None; }
    Some(LlamaProgress { message: "Downloading model".to_owned(), ratio: Some(done / total), detail: Some(format!("{} / {}", format_bytes(done), format_bytes(total))) })
}

pub fn format_bytes(bytes: f64) -> String {
    if bytes < 1024.0 { return format!("{bytes} B"); }
    let units = ["KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes / 1024.0;
    let mut unit = units[0];
    let mut index = 1;
    while index < units.len() && value >= 1024.0 {
        value /= 1024.0;
        unit = units[index];
        index += 1;
    }
    if value >= 10.0 { format!("{value:.1} {unit}") } else { format!("{value:.2} {unit}") }
}

pub fn normalize_llama_server_url(value: &str) -> Result<String, String> {
    let mut url = reqwest::Url::parse(value.trim()).map_err(|error| format!("Invalid server URL: {error}"))?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err("Server URL must use http or https".to_owned());
    }
    url.set_fragment(None);
    url.set_query(None);
    let trimmed = url.path().trim_end_matches('/');
    let stripped = trimmed.strip_suffix("/v1").unwrap_or(trimmed);
    url.set_path(if stripped.is_empty() { "/" } else { stripped });
    Ok(url.to_string().trim_end_matches('/').to_owned())
}

pub fn llama_inference_url(server_url: &str) -> Result<String, String> {
    Ok(format!("{}/v1", normalize_llama_server_url(server_url)?))
}

async fn signal_cancelled(signal: Option<&AbortSignal>) {
    match signal {
        Some(signal) => signal.cancelled().await,
        None => std::future::pending::<()>().await,
    }
}

fn apply_load_event(event: &LlamaModelEvent, model: &str, loaded: &mut bool, error: &mut Option<String>, on_progress: &mut dyn FnMut(LlamaProgress)) {
    if event.model != model { return; }
    if event.event != "model_status" && event.event != "status_change" { return; }
    let status = event.data.as_ref().and_then(|data| data.get("status")).and_then(Value::as_str);
    if status == Some("loaded") { *loaded = true; }
    if status == Some("unloaded") { *error = Some("Model failed to load".to_owned()); }
    if let Some(progress) = event.data.as_ref().and_then(parse_load_progress) { on_progress(progress); }
}

fn apply_download_event(event: &LlamaModelEvent, model: &str, finished: &mut bool, failure: &mut Option<String>, saw_downloading: &mut bool, on_progress: &mut dyn FnMut(LlamaProgress)) {
    if event.model != model { return; }
    if event.event == "download_finished" { *finished = true; }
    if event.event == "download_failed" {
        *failure = Some(event.data.as_ref().map_or_else(|| "Download failed".to_owned(), |data| error_message(data, "Download failed")));
    }
    if event.event == "download_progress" {
        *saw_downloading = true;
        if let Some(progress) = event.data.as_ref().and_then(parse_download_progress) { on_progress(progress); }
    }
}

#[derive(Clone)]
pub struct LlamaClient {
    pub server_url: String,
    api_key: Option<String>,
    http: reqwest::Client,
}

impl LlamaClient {
    pub fn new(server_url: &str, api_key: Option<&str>) -> Result<Self, String> {
        Ok(Self {
            server_url: normalize_llama_server_url(server_url)?,
            api_key: api_key.filter(|key| !key.is_empty()).map(str::to_owned),
            http: reqwest::Client::new(),
        })
    }

    async fn request(&self, method: reqwest::Method, path: &str, body: Option<Value>, signal: Option<&AbortSignal>) -> Result<Value, String> {
        let mut request = self.http.request(method, format!("{}{}", self.server_url, path)).timeout(Duration::from_millis(REQUEST_TIMEOUT_MS));
        if body.is_some() { request = request.header("content-type", "application/json"); }
        if let Some(key) = &self.api_key { request = request.header("authorization", format!("Bearer {key}")); }
        if let Some(body) = body { request = request.json(&body); }
        let response = tokio::select! {
            biased;
            _ = signal_cancelled(signal) => return Err("Cancelled".to_owned()),
            result = request.send() => result.map_err(|error| format!("llama.cpp request failed: {error}"))?,
        };
        let status = response.status();
        let payload = tokio::select! {
            biased;
            _ = signal_cancelled(signal) => return Err("Cancelled".to_owned()),
            result = response.json::<Value>() => result.unwrap_or(Value::Null),
        };
        if !status.is_success() {
            return Err(error_message(&payload, &format!("llama.cpp returned HTTP {}", status.as_u16())));
        }
        Ok(payload)
    }

    pub async fn list(&self, reload: bool, signal: Option<&AbortSignal>) -> Result<Vec<LlamaModelInfo>, String> {
        let payload = self.request(reqwest::Method::GET, if reload { "/models?reload=1" } else { "/models" }, None, signal).await?;
        let Some(data) = payload.get("data").and_then(Value::as_array) else {
            return Err("llama.cpp returned an invalid model catalog".to_owned());
        };
        if !data.iter().all(is_model_info) {
            return Err("Server is not running in llama.cpp router mode".to_owned());
        }
        serde_json::from_value(Value::Array(data.clone())).map_err(|error| error.to_string())
    }

    pub async fn props(&self, signal: Option<&AbortSignal>) -> Result<LlamaServerProps, String> {
        let payload = self.request(reqwest::Method::GET, "/props", None, signal).await?;
        Ok(LlamaServerProps { models_autoload: payload.get("models_autoload").and_then(Value::as_bool) })
    }

    pub async fn load(&self, model: &str, signal: Option<&AbortSignal>) -> Result<(), String> {
        self.request(reqwest::Method::POST, "/models/load", Some(serde_json::json!({ "model": model })), signal).await.map(|_| ())
    }

    pub async fn unload(&self, model: &str, signal: Option<&AbortSignal>) -> Result<(), String> {
        self.request(reqwest::Method::POST, "/models/unload", Some(serde_json::json!({ "model": model })), signal).await.map(|_| ())
    }

    pub async fn download(&self, model: &str, signal: Option<&AbortSignal>) -> Result<(), String> {
        self.request(reqwest::Method::POST, "/models", Some(serde_json::json!({ "model": model })), signal).await.map(|_| ())
    }

    pub async fn unload_and_wait(&self, model: &str, signal: Option<&AbortSignal>) -> Result<(), String> {
        self.unload(model, signal).await?;
        loop {
            if signal.is_some_and(AbortSignal::aborted) { return Err("Cancelled".to_owned()); }
            let entry = self.list(false, signal).await?.into_iter().find(|candidate| candidate.id == model);
            if entry.is_none_or(|entry| entry.status.value == "unloaded") { return Ok(()); }
            tokio::select! {
                biased;
                _ = signal_cancelled(signal) => return Err("Cancelled".to_owned()),
                _ = tokio::time::sleep(Duration::from_millis(100)) => {}
            }
        }
    }

    /// Subscribes to the SSE stream first; the oneshot resolves once connected, so the caller triggers the load/download only after the subscription is live.
    fn subscribe(&self, signal: Option<&AbortSignal>) -> (mpsc::UnboundedReceiver<LlamaModelEvent>, oneshot::Receiver<Result<(), String>>) {
        use futures_util::StreamExt;
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let (ready_tx, ready_rx) = oneshot::channel();
        let http = self.http.clone();
        let url = format!("{}/models/sse", self.server_url);
        let api_key = self.api_key.clone();
        let signal = signal.cloned();
        tokio::spawn(async move {
            let mut request = http.get(url).timeout(Duration::from_millis(REQUEST_TIMEOUT_MS));
            if let Some(key) = &api_key { request = request.header("authorization", format!("Bearer {key}")); }
            let response = tokio::select! {
                biased;
                _ = events_tx.closed() => return,
                _ = signal_cancelled(signal.as_ref()) => return,
                result = request.send() => match result {
                    Ok(response) => response,
                    Err(error) => { let _ = ready_tx.send(Err(format!("llama.cpp SSE failed: {error}"))); return; }
                },
            };
            if !response.status().is_success() {
                let _ = ready_tx.send(Err(format!("llama.cpp SSE returned HTTP {}", response.status().as_u16())));
                return;
            }
            let _ = ready_tx.send(Ok(()));
            let mut stream = response.bytes_stream();
            let mut raw: Vec<u8> = Vec::new();
            let mut text = String::new();
            loop {
                let chunk = tokio::select! {
                    biased;
                    _ = events_tx.closed() => break,
                    _ = signal_cancelled(signal.as_ref()) => break,
                    chunk = stream.next() => chunk,
                };
                let Some(chunk) = chunk else { break; };
                let Ok(chunk) = chunk else { break; };
                raw.extend_from_slice(&chunk);
                match std::str::from_utf8(&raw) {
                    Ok(decoded) => { text.push_str(decoded); raw.clear(); }
                    Err(error) => {
                        let valid = error.valid_up_to();
                        text.push_str(std::str::from_utf8(&raw[..valid]).expect("valid utf8 prefix"));
                        raw.drain(..valid);
                    }
                }
                if text.contains('\r') { text = text.replace("\r\n", "\n"); }
                while let Some(boundary) = text.find("\n\n") {
                    let frame: String = text.drain(..boundary + 2).collect();
                    let data: String = frame.lines().filter_map(|line| line.strip_prefix("data:")).map(|line| line.trim_start()).collect::<Vec<_>>().join("\n");
                    if data.is_empty() { continue; }
                    if let Ok(event) = serde_json::from_str::<LlamaModelEvent>(&data)
                        && !event.model.is_empty() && !event.event.is_empty()
                        && events_tx.send(event).is_err()
                    {
                        return;
                    }
                }
            }
        });
        (events_rx, ready_rx)
    }

    pub async fn load_and_wait(&self, model: &str, on_progress: &mut dyn FnMut(LlamaProgress), signal: Option<&AbortSignal>) -> Result<LlamaModelInfo, String> {
        let (mut events, ready) = self.subscribe(signal);
        tokio::select! {
            biased;
            _ = signal_cancelled(signal) => return Err("Cancelled".to_owned()),
            result = ready => result.map_err(|_| "llama.cpp SSE subscription closed".to_owned())??,
        }
        self.load(model, signal).await?;
        on_progress(LlamaProgress { message: "Loading model".to_owned(), ..Default::default() });
        let mut event_loaded = false;
        let mut event_error: Option<String> = None;
        loop {
            while let Ok(event) = events.try_recv() { apply_load_event(&event, model, &mut event_loaded, &mut event_error, on_progress); }
            if signal.is_some_and(AbortSignal::aborted) { return Err("Cancelled".to_owned()); }
            let entry = self.list(false, signal).await?.into_iter().find(|candidate| candidate.id == model);
            if entry.as_ref().is_some_and(|entry| entry.status.value == "loaded") { return Ok(entry.expect("checked")); }
            if event_loaded && entry.is_none() {
                return Ok(LlamaModelInfo { id: model.to_owned(), status: LlamaModelState { value: "loaded".to_owned(), ..Default::default() }, ..Default::default() });
            }
            if entry.as_ref().is_some_and(|entry| entry.status.failed == Some(true)) || event_error.is_some() {
                let exit_code = entry.and_then(|entry| entry.status.exit_code);
                return Err(match exit_code {
                    None => event_error.unwrap_or_else(|| "Model failed to load".to_owned()),
                    Some(code) => format!("Model exited with code {code}"),
                });
            }
            tokio::select! {
                biased;
                _ = signal_cancelled(signal) => return Err("Cancelled".to_owned()),
                event = events.recv() => { if let Some(event) = event { apply_load_event(&event, model, &mut event_loaded, &mut event_error, on_progress); } }
                _ = tokio::time::sleep(Duration::from_millis(250)) => {}
            }
        }
    }

    pub async fn download_and_wait(&self, model: &str, on_progress: &mut dyn FnMut(LlamaProgress), signal: Option<&AbortSignal>) -> Result<Vec<LlamaModelInfo>, String> {
        let (mut events, ready) = self.subscribe(signal);
        tokio::select! {
            biased;
            _ = signal_cancelled(signal) => return Err("Cancelled".to_owned()),
            result = ready => result.map_err(|_| "llama.cpp SSE subscription closed".to_owned())??,
        }
        self.download(model, signal).await?;
        on_progress(LlamaProgress { message: "Downloading model".to_owned(), ..Default::default() });
        let mut finished = false;
        let mut failure: Option<String> = None;
        let mut saw_downloading = false;
        let mut polls = 0;
        loop {
            while let Ok(event) = events.try_recv() { apply_download_event(&event, model, &mut finished, &mut failure, &mut saw_downloading, on_progress); }
            if let Some(failure) = &failure { return Err(failure.clone()); }
            if signal.is_some_and(AbortSignal::aborted) { return Err("Cancelled".to_owned()); }
            let models = self.list(false, signal).await?;
            polls += 1;
            let entry = models.iter().find(|candidate| candidate.id == model).cloned();
            if entry.as_ref().is_some_and(|entry| entry.status.value == "downloading") {
                saw_downloading = true;
                if let Some(progress) = entry.as_ref().and_then(|entry| entry.status.progress.as_ref()).and_then(parse_download_progress) { on_progress(progress); }
            } else if finished || (entry.is_some() && (saw_downloading || polls >= 2)) {
                return self.list(true, signal).await;
            }
            tokio::select! {
                biased;
                _ = signal_cancelled(signal) => return Err("Cancelled".to_owned()),
                event = events.recv() => { if let Some(event) = event { apply_download_event(&event, model, &mut finished, &mut failure, &mut saw_downloading, on_progress); } }
                _ = tokio::time::sleep(Duration::from_millis(500)) => {}
            }
        }
    }
}

pub fn progress_map(entries: &BTreeMap<String, Value>) -> Option<LlamaProgress> {
    parse_download_progress(&Value::Object(entries.clone().into_iter().collect()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn read_head(stream: &mut tokio::net::TcpStream) -> String {
        let mut buffer = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            match stream.read(&mut byte).await {
                Ok(0) | Err(_) => break,
                Ok(_) => buffer.push(byte[0]),
            }
            if buffer.ends_with(b"\r\n\r\n") { break; }
        }
        String::from_utf8_lossy(&buffer).into_owned()
    }

    fn http_ok(body: &str) -> String {
        format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body)
    }

    #[test]
    fn normalize_strips_v1_and_trailing_slash() {
        assert_eq!(normalize_llama_server_url("http://127.0.0.1:8080/").unwrap(), "http://127.0.0.1:8080");
        assert_eq!(normalize_llama_server_url("http://127.0.0.1:8080/v1").unwrap(), "http://127.0.0.1:8080");
        assert_eq!(llama_inference_url("http://127.0.0.1:8080").unwrap(), "http://127.0.0.1:8080/v1");
    }

    #[test]
    fn normalize_rejects_non_http_schemes() {
        assert!(normalize_llama_server_url("ftp://example.com").is_err());
    }

    #[test]
    fn format_bytes_matches_the_pinned_thresholds() {
        assert_eq!(format_bytes(512.0), "512 B");
        assert_eq!(format_bytes(2048.0), "2.00 KiB");
        assert_eq!(format_bytes(15360.0), "15.0 KiB");
    }

    #[test]
    fn download_progress_sums_every_file() {
        let mut map = BTreeMap::new();
        map.insert("a".to_owned(), serde_json::json!({ "done": 1, "total": 2 }));
        map.insert("b".to_owned(), serde_json::json!({ "done": 1, "total": 2 }));
        assert_eq!(progress_map(&map).unwrap().ratio, Some(0.5));
        assert!(progress_map(&BTreeMap::new()).is_none());
    }

    #[test]
    fn router_validation_requires_id_and_status() {
        assert!(is_model_info(&serde_json::json!({ "id": "m", "status": { "value": "loaded" } })));
        assert!(!is_model_info(&serde_json::json!({ "id": "m" })));
        assert!(!is_model_info(&serde_json::json!({ "status": { "value": "loaded" } })));
    }

    #[test]
    fn load_event_sets_loaded_and_error_and_progress() {
        let (mut loaded, mut error) = (false, None);
        let mut progress = Vec::new();
        let mut sink = |value: LlamaProgress| progress.push(value.message);
        let mut event = LlamaModelEvent { model: "m".to_owned(), event: "model_status".to_owned(), data: Some(serde_json::json!({ "status": "loaded" })) };
        apply_load_event(&event, "m", &mut loaded, &mut error, &mut sink);
        assert!(loaded);
        event.data = Some(serde_json::json!({ "status": "unloaded" }));
        apply_load_event(&event, "m", &mut loaded, &mut error, &mut sink);
        assert_eq!(error.as_deref(), Some("Model failed to load"));
        let other = LlamaModelEvent { model: "other".to_owned(), event: "model_status".to_owned(), data: Some(serde_json::json!({ "status": "loaded" })) };
        let (mut ignored, mut none) = (false, None);
        apply_load_event(&other, "m", &mut ignored, &mut none, &mut sink);
        assert!(!ignored);
    }

    #[test]
    fn download_event_records_finish_failure_and_progress() {
        let (mut finished, mut failure, mut saw) = (false, None, false);
        let mut progress = Vec::new();
        let mut sink = |value: LlamaProgress| progress.push(value);
        apply_download_event(&LlamaModelEvent { model: "m".to_owned(), event: "download_progress".to_owned(), data: Some(serde_json::json!({ "a": { "done": 1, "total": 4 } })) }, "m", &mut finished, &mut failure, &mut saw, &mut sink);
        assert!(saw);
        assert_eq!(progress[0].ratio, Some(0.25));
        apply_download_event(&LlamaModelEvent { model: "m".to_owned(), event: "download_failed".to_owned(), data: Some(serde_json::json!({ "error": { "message": "boom" } })) }, "m", &mut finished, &mut failure, &mut saw, &mut sink);
        assert_eq!(failure.as_deref(), Some("boom"));
        apply_download_event(&LlamaModelEvent { model: "m".to_owned(), event: "download_finished".to_owned(), data: None }, "m", &mut finished, &mut failure, &mut saw, &mut sink);
        assert!(finished);
    }

    #[tokio::test]
    async fn load_triggers_the_post_only_after_the_sse_subscription_is_live() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let order = Arc::new(Mutex::new(Vec::<String>::new()));
        let server_order = order.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else { break; };
                let order = server_order.clone();
                tokio::spawn(async move {
                    let head = read_head(&mut stream).await;
                    let line = head.lines().next().unwrap_or("").to_owned();
                    if line.contains("/models/sse") {
                        order.lock().unwrap().push("sse".to_owned());
                        let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n").await;
                        let _ = stream.flush().await;
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        let _ = stream.write_all(b"data: {\"model\":\"m\",\"event\":\"model_status\",\"data\":{\"status\":\"loaded\"}}\n\n").await;
                        let _ = stream.flush().await;
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    } else if line.starts_with("POST /models/load") {
                        order.lock().unwrap().push("load".to_owned());
                        let _ = stream.write_all(http_ok("{}").as_bytes()).await;
                    } else if line.starts_with("GET /models") {
                        order.lock().unwrap().push("list".to_owned());
                        let _ = stream.write_all(http_ok(r#"{"data":[{"id":"m","status":{"value":"loaded"}}]}"#).as_bytes()).await;
                    }
                });
            }
        });
        let client = LlamaClient::new(&format!("http://{addr}"), None).expect("client");
        let controller = maho_ai::utils::abort::AbortController::new();
        let mut progress = Vec::new();
        let model = tokio::time::timeout(Duration::from_secs(3), client.load_and_wait("m", &mut |p| progress.push(p), Some(&controller.signal()))).await.expect("bounded").expect("load");
        assert_eq!(model.id, "m");
        let order = order.lock().unwrap().clone();
        assert_eq!(&order[..2], &["sse".to_owned(), "load".to_owned()], "POST must follow the SSE subscription: {order:?}");
    }

    #[tokio::test]
    async fn subscribe_aborts_while_sse_headers_are_held() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let Ok((mut stream, _)) = listener.accept().await else { return; };
            let _ = read_head(&mut stream).await;
            tokio::time::sleep(Duration::from_secs(30)).await;
        });
        let client = LlamaClient::new(&format!("http://{addr}"), None).expect("client");
        let controller = maho_ai::utils::abort::AbortController::new();
        let signal = controller.signal();
        tokio::spawn(async move { tokio::time::sleep(Duration::from_millis(100)).await; controller.abort(None); });
        let mut progress = Vec::new();
        let result = tokio::time::timeout(Duration::from_secs(3), client.load_and_wait("m", &mut |p| progress.push(p), Some(&signal))).await.expect("bounded");
        assert_eq!(result.unwrap_err(), "Cancelled");
    }

    #[tokio::test]
    async fn sse_reassembles_a_frame_split_across_chunks_and_utf8() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else { break; };
                tokio::spawn(async move {
                    let head = read_head(&mut stream).await;
                    let line = head.lines().next().unwrap_or("").to_owned();
                    if line.contains("/models/sse") {
                        let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n").await;
                        let _ = stream.flush().await;
                        let frame = "data: {\"model\":\"m\",\"event\":\"model_status\",\"data\":{\"status\":\"loaded\",\"note\":\"로딩\"}}\n\n";
                        let bytes = frame.as_bytes();
                        let split = frame.find('로').expect("char") + 1;
                        let _ = stream.write_all(&bytes[..split]).await;
                        let _ = stream.flush().await;
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        let _ = stream.write_all(&bytes[split..]).await;
                        let _ = stream.flush().await;
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    } else if line.starts_with("GET /models") {
                        let _ = stream.write_all(http_ok(r#"{"data":[]}"#).as_bytes()).await;
                    }
                });
            }
        });
        let client = LlamaClient::new(&format!("http://{addr}"), None).expect("client");
        let controller = maho_ai::utils::abort::AbortController::new();
        let mut progress = Vec::new();
        let model = tokio::time::timeout(Duration::from_secs(3), client.load_and_wait("m", &mut |p| progress.push(p), Some(&controller.signal()))).await.expect("bounded").expect("load");
        assert_eq!(model.status.value, "loaded", "the split SSE frame must decode to the loaded event");
    }
}
