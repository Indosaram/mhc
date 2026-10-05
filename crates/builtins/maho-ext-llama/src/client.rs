//! Port of `packages/coding-agent/src/extensions/llama/client.ts` (pin `fe8c564b`).

use std::collections::BTreeMap;
use std::time::Duration;

use maho_ai::utils::abort::AbortSignal;
use serde::{Deserialize, Serialize};
use serde_json::Value;

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
        let send = request.send();
        let response = match signal {
            Some(signal) => tokio::select! {
                biased;
                _ = signal.cancelled() => return Err("Cancelled".to_owned()),
                result = send => result.map_err(|error| format!("llama.cpp request failed: {error}"))?,
            },
            None => send.await.map_err(|error| format!("llama.cpp request failed: {error}"))?,
        };
        let status = response.status();
        let payload: Value = response.json().await.unwrap_or(Value::Null);
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
            sleep(100, signal).await?;
        }
    }

    pub async fn load_and_wait(&self, model: &str, on_progress: &mut dyn FnMut(LlamaProgress), signal: Option<&AbortSignal>) -> Result<LlamaModelInfo, String> {
        let mut event_loaded = false;
        let mut event_error: Option<String> = None;
        let watcher_signal = signal.cloned();
        let watch = self.watch(&mut |event: LlamaModelEvent| {
            if event.model != model { return; }
            if event.event != "model_status" && event.event != "status_change" { return; }
            let status = event.data.as_ref().and_then(|data| data.get("status")).and_then(Value::as_str);
            if status == Some("loaded") { event_loaded = true; }
            if status == Some("unloaded") { event_error = Some("Model failed to load".to_owned()); }
            if let Some(progress) = event.data.as_ref().and_then(parse_load_progress) { on_progress(progress); }
        }, watcher_signal.as_ref());
        tokio::pin!(watch);
        let mut watch = watch;
        self.load(model, signal).await?;
        on_progress(LlamaProgress { message: "Loading model".to_owned(), ..Default::default() });
        loop {
            if signal.is_some_and(AbortSignal::aborted) { return Err("Cancelled".to_owned()); }
            let entry = self.list(false, signal).await?.into_iter().find(|candidate| candidate.id == model);
            if entry.as_ref().is_some_and(|entry| entry.status.value == "loaded") { return Ok(entry.expect("checked")); }
            if event_loaded && entry.is_none() { return Ok(LlamaModelInfo { id: model.to_owned(), status: LlamaModelState { value: "loaded".to_owned(), ..Default::default() }, ..Default::default() }); }
            if entry.as_ref().is_some_and(|entry| entry.status.failed == Some(true)) || event_error.is_some() {
                let exit_code = entry.and_then(|entry| entry.status.exit_code);
                return Err(match exit_code {
                    None => event_error.unwrap_or_else(|| "Model failed to load".to_owned()),
                    Some(code) => format!("Model exited with code {code}"),
                });
            }
            sleep(250, signal).await?;
        }
    }

    pub async fn download_and_wait(&self, model: &str, on_progress: &mut dyn FnMut(LlamaProgress), signal: Option<&AbortSignal>) -> Result<Vec<LlamaModelInfo>, String> {
        let mut finished = false;
        let mut failure: Option<String> = None;
        let mut saw_downloading = false;
        let mut polls = 0;
        let mut event_error: Option<String> = None;
        let mut event_progress: Option<LlamaProgress> = None;
        {
            let mut watch = self.watch(&mut |event: LlamaModelEvent| {
                if event.model != model { return; }
                if event.event == "download_finished" { finished = true; }
                if event.event == "download_failed" { failure = Some(event.data.as_ref().map_or_else(|| "Download failed".to_owned(), |data| error_message(data, "Download failed"))); }
                if event.event == "download_progress" {
                    saw_downloading = true;
                    event_progress = event.data.as_ref().and_then(parse_download_progress);
                }
            }, signal).await?;
            let _ = &mut watch;
        }
        let _ = event_error;
        self.download(model, signal).await?;
        on_progress(LlamaProgress { message: "Downloading model".to_owned(), ..Default::default() });
        loop {
            if signal.is_some_and(AbortSignal::aborted) { return Err("Cancelled".to_owned()); }
            if let Some(failure) = &failure { return Err(failure.clone()); }
            if let Some(progress) = event_progress.take() { on_progress(progress); }
            let models = self.list(false, signal).await?;
            polls += 1;
            let entry = models.iter().find(|candidate| candidate.id == model).cloned();
            if entry.as_ref().is_some_and(|entry| entry.status.value == "downloading") {
                saw_downloading = true;
                if let Some(progress) = entry.as_ref().and_then(|entry| entry.status.progress.as_ref()).and_then(parse_download_progress) { on_progress(progress); }
            } else if finished || (entry.is_some() && (saw_downloading || polls >= 2)) {
                return self.list(true, signal).await;
            }
            sleep(500, signal).await?;
        }
    }

    async fn watch(&self, on_event: &mut dyn FnMut(LlamaModelEvent), signal: Option<&AbortSignal>) -> Result<(), String> {
        use futures_util::StreamExt;
        let mut request = self.http.get(format!("{}/models/sse", self.server_url)).timeout(Duration::from_millis(REQUEST_TIMEOUT_MS));
        if let Some(key) = &self.api_key { request = request.header("authorization", format!("Bearer {key}")); }
        let response = request.send().await.map_err(|error| format!("llama.cpp SSE failed: {error}"))?;
        if !response.status().is_success() { return Err(format!("llama.cpp SSE returned HTTP {}", response.status().as_u16())); }
        let mut stream = response.bytes_stream();
        let mut buffer = String::new();
        loop {
            let next = match signal {
                Some(signal) => tokio::select! { biased; _ = signal.cancelled() => return Ok(()), chunk = stream.next() => chunk },
                None => stream.next().await,
            };
            let Some(chunk) = next else { break; };
            let chunk = chunk.map_err(|error| format!("llama.cpp SSE stream failed: {error}"))?;
            buffer.push_str(&String::from_utf8_lossy(&chunk).replace("\r\n", "\n"));
            while let Some(boundary) = buffer.find("\n\n") {
                let frame: String = buffer.drain(..boundary + 2).collect();
                let data: String = frame.lines().filter_map(|line| line.strip_prefix("data:")).map(|line| line.trim_start()).collect::<Vec<_>>().join("\n");
                if data.is_empty() { continue; }
                if let Ok(event) = serde_json::from_str::<LlamaModelEvent>(&data)
                    && !event.model.is_empty() && !event.event.is_empty() {
                    on_event(event);
                }
            }
        }
        Ok(())
    }
}

async fn sleep(ms: u64, signal: Option<&AbortSignal>) -> Result<(), String> {
    match signal {
        Some(signal) => tokio::select! { biased; _ = signal.cancelled() => Err("Cancelled".to_owned()), _ = tokio::time::sleep(Duration::from_millis(ms)) => Ok(()) },
        None => { tokio::time::sleep(Duration::from_millis(ms)).await; Ok(()) }
    }
}

pub fn progress_map(entries: &BTreeMap<String, Value>) -> Option<LlamaProgress> {
    parse_download_progress(&Value::Object(entries.clone().into_iter().collect()))
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
