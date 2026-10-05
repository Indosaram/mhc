//! Port of `packages/coding-agent/src/extensions/llama/huggingface.ts` (pin `fe8c564b`).

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use maho_ai::utils::abort::AbortSignal;
use serde_json::Value;

pub const DEFAULT_HUGGING_FACE_URL: &str = "https://huggingface.co";
const REQUEST_TIMEOUT_MS: u64 = 15_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HuggingFaceModel {
    pub id: String,
    pub downloads: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HuggingFaceQuantization {
    pub name: String,
    pub size: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HuggingFaceModelDetails {
    pub id: String,
    pub gated: Option<String>,
    pub quantizations: Vec<HuggingFaceQuantization>,
}

fn payload_error(payload: &Value, fallback: &str) -> String {
    payload.get("error").and_then(Value::as_str).filter(|error| !error.is_empty()).unwrap_or(fallback).to_owned()
}

fn parse_rate_limit_delay(value: Option<&str>) -> Option<u64> {
    let value = value?;
    value.split(';').find_map(|part| part.strip_prefix("t=")).and_then(|value| value.parse().ok())
}

fn read_token(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok().map(|token| token.trim().to_owned()).filter(|token| !token.is_empty())
}

pub fn find_hugging_face_token(env: &BTreeMap<String, String>) -> Option<String> {
    if let Some(token) = env.get("HF_TOKEN").map(|value| value.trim()).filter(|value| !value.is_empty()) {
        return Some(token.to_owned());
    }
    let home = env.get("HOME").cloned().unwrap_or_default();
    let paths = [
        env.get("HF_TOKEN_PATH").map(PathBuf::from),
        env.get("HF_HOME").map(|home| PathBuf::from(home).join("token")),
        env.get("XDG_CACHE_HOME").map(|cache| PathBuf::from(cache).join("huggingface/token")),
        (!home.is_empty()).then(|| PathBuf::from(&home).join(".cache/huggingface/token")),
    ];
    let mut seen = HashSet::new();
    for path in paths.into_iter().flatten() {
        if !seen.insert(path.clone()) { continue; }
        if let Some(token) = read_token(&path) { return Some(token); }
    }
    None
}

fn quantization_of(filename: &str) -> Option<String> {
    let stem = filename.strip_suffix(".gguf")?;
    let stem = shard_suffix_stripped(stem);
    for (index, _) in stem.char_indices() {
        if index > 0 && !matches!(stem.as_bytes()[index - 1], b'-' | b'_' | b'.') { continue; }
        let token = &stem[index..];
        if quant_grammar(token) { return Some(token.to_ascii_uppercase()); }
    }
    None
}

fn quant_grammar(token: &str) -> bool {
    let upper = token.to_ascii_uppercase();
    let upper = upper.strip_prefix("UD-").unwrap_or(&upper);
    if matches!(upper, "BF16" | "F16" | "F32") { return true; }
    let tail = |rest: &str, min_parts: usize| {
        let mut parts = rest.split('_');
        let Some(first) = parts.next() else { return false; };
        if !first.is_empty() { return false; }
        let mut count = 0;
        for part in parts {
            if part.is_empty() || !part.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()) { return false; }
            count += 1;
        }
        count >= min_parts
    };
    let digit_lead = |rest: &str| rest.as_bytes().first().is_some_and(u8::is_ascii_digit);
    if let Some(rest) = upper.strip_prefix("IQ") { return digit_lead(rest) && tail(&rest[1..], 1); }
    if let Some(rest) = upper.strip_prefix("MXFP") { return digit_lead(rest) && tail(&rest[1..], 0); }
    if let Some(rest) = upper.strip_prefix('Q') { return digit_lead(rest) && tail(&rest[1..], 1); }
    false
}

fn shard_suffix_stripped(stem: &str) -> String {
    let bytes = stem.as_bytes();
    let Some(dash) = stem.rfind("-of-") else { return stem.to_owned(); };
    let left_start = dash.checked_sub(5).filter(|start| *start > 0 && bytes[*start - 1] == b'-');
    let right_end = dash + 4 + 5;
    let Some(left_start) = left_start else { return stem.to_owned(); };
    let all_digits = |slice: &[u8]| !slice.is_empty() && slice.iter().all(u8::is_ascii_digit);
    if right_end <= bytes.len()
        && all_digits(&bytes[left_start..dash])
        && all_digits(&bytes[dash + 4..right_end])
    {
        stem[..left_start - 1].to_owned()
    } else {
        stem.to_owned()
    }
}

fn is_quantization_token(token: &str) -> bool { quant_grammar(token) }

pub struct HuggingFaceClient {
    token: Option<String>,
    base_url: String,
    http: reqwest::Client,
}

impl HuggingFaceClient {
    pub fn new(token: Option<String>, base_url: Option<&str>) -> Self {
        Self {
            token: token.filter(|token| !token.is_empty()),
            base_url: base_url.unwrap_or(DEFAULT_HUGGING_FACE_URL).trim_end_matches('/').to_owned(),
            http: reqwest::Client::new(),
        }
    }

    fn request(&self, path: &str, signal: Option<&AbortSignal>) -> futures_util::future::BoxFuture<'_, Result<Value, String>> {
        Box::pin(async move {
            let mut request = self.http.get(format!("{}{}", self.base_url, path)).timeout(Duration::from_millis(REQUEST_TIMEOUT_MS));
            if let Some(token) = &self.token { request = request.header("authorization", format!("Bearer {token}")); }
            let response = tokio::select! {
                biased;
                _ = signal_cancelled(signal) => return Err("Cancelled".to_owned()),
                result = request.send() => result.map_err(|error| format!("Hugging Face request failed: {error}"))?,
            };
            let status = response.status();
            let retry_after = response.headers().get("retry-after").and_then(|value| value.to_str().ok()).map(str::to_owned);
            let ratelimit = response.headers().get("ratelimit").and_then(|value| value.to_str().ok()).map(str::to_owned);
            let payload = tokio::select! {
                biased;
                _ = signal_cancelled(signal) => return Err("Cancelled".to_owned()),
                result = response.json::<Value>() => result.unwrap_or(Value::Null),
            };
            if !status.is_success() {
                let fallback = format!("Hugging Face returned HTTP {}", status.as_u16());
                if status.as_u16() == 429 {
                    let delay = retry_after.and_then(|value| value.parse().ok()).or_else(|| parse_rate_limit_delay(ratelimit.as_deref()));
                    return Err(match delay {
                        Some(delay) => format!("Hugging Face rate limit reached; retry in {delay}s"),
                        None => "Hugging Face rate limit reached".to_owned(),
                    });
                }
                return Err(payload_error(&payload, &fallback));
            }
            Ok(payload)
        })
    }

    pub async fn search(&self, query: &str, signal: Option<&AbortSignal>) -> Result<Vec<HuggingFaceModel>, String> {
        let path = format!("/api/models?search={}&filter=gguf&sort=downloads&direction=-1&limit=20", urlencode(query));
        let payload = self.request(&path, signal).await?;
        let Some(items) = payload.as_array() else { return Err("Hugging Face returned invalid search results".to_owned()); };
        Ok(items.iter().filter_map(|value| {
            let id = value.get("id").and_then(Value::as_str)?.to_owned();
            let downloads = value.get("downloads").and_then(Value::as_u64).unwrap_or(0);
            Some(HuggingFaceModel { id, downloads })
        }).collect())
    }

    pub async fn details(&self, id: &str, signal: Option<&AbortSignal>) -> Result<HuggingFaceModelDetails, String> {
        let encoded = id.split('/').map(urlencode).collect::<Vec<_>>().join("/");
        let payload = self.request(&format!("/api/models/{encoded}?blobs=true"), signal).await?;
        if !payload.is_object() { return Err("Hugging Face returned invalid model details".to_owned()); }
        let mut sizes: BTreeMap<String, (u64, bool)> = BTreeMap::new();
        if let Some(siblings) = payload.get("siblings").and_then(Value::as_array) {
            for value in siblings {
                let Some(filename) = value.get("rfilename").and_then(Value::as_str) else { continue; };
                if !filename.to_ascii_lowercase().ends_with(".gguf") { continue; }
                let base = filename.rsplit('/').next().unwrap_or(filename);
                if base.to_ascii_lowercase().starts_with("mmproj") { continue; }
                let Some(quantization) = quantization_of(base) else { continue; };
                let entry = sizes.entry(quantization).or_insert((0, true));
                match value.get("size").and_then(Value::as_u64) {
                    Some(size) => entry.0 += size,
                    None => entry.1 = false,
                }
            }
        }
        let mut quantizations: Vec<HuggingFaceQuantization> = sizes.into_iter()
            .map(|(name, (total, complete))| HuggingFaceQuantization { name, size: complete.then_some(total) })
            .collect();
        quantizations.sort_by(|left, right| {
            if left.name == "Q4_K_M" { return std::cmp::Ordering::Less; }
            if right.name == "Q4_K_M" { return std::cmp::Ordering::Greater; }
            left.size.unwrap_or(u64::MAX).cmp(&right.size.unwrap_or(u64::MAX)).then_with(|| left.name.cmp(&right.name))
        });
        let gated = match payload.get("gated").and_then(Value::as_str) {
            Some("auto") => Some("auto".to_owned()),
            Some("manual") => Some("manual".to_owned()),
            _ => None,
        };
        Ok(HuggingFaceModelDetails {
            id: payload.get("id").and_then(Value::as_str).unwrap_or(id).to_owned(),
            gated,
            quantizations,
        })
    }
}

fn urlencode(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

async fn signal_cancelled(signal: Option<&AbortSignal>) {
    match signal {
        Some(signal) => signal.cancelled().await,
        None => std::future::pending::<()>().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantization_tokens_match_the_pinned_pattern() {
        assert_eq!(quantization_of("Model-Q4_K_M.gguf").as_deref(), Some("Q4_K_M"));
        assert_eq!(quantization_of("Model-00001-of-00002-Q4_K_M.gguf").as_deref(), Some("Q4_K_M"));
        assert_eq!(quantization_of("Model-00001-of-00002-BF16.gguf").as_deref(), Some("BF16"));
        assert_eq!(quantization_of("Model-BF16.gguf").as_deref(), Some("BF16"));
        assert_eq!(quantization_of("Model-IQ4_XS.gguf").as_deref(), Some("IQ4_XS"));
        assert_eq!(quantization_of("Model-MXFP4.gguf").as_deref(), Some("MXFP4"));
        assert_eq!(quantization_of("Model-Q4.gguf"), None);
        assert_eq!(quantization_of("Model-README.gguf"), None);
    }

    #[test]
    fn token_env_wins_over_files() {
        let mut env = BTreeMap::new();
        env.insert("HF_TOKEN".to_owned(), "  spaced  ".to_owned());
        assert_eq!(find_hugging_face_token(&env).as_deref(), Some("spaced"));
    }
}
