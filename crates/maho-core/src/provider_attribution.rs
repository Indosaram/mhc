//! Port of senpi packages/coding-agent/src/core/provider-attribution.ts.

use std::collections::BTreeMap;

use maho_ai::model::Model;

use crate::config::app_name;
use crate::settings_manager::SettingsManager;
use crate::telemetry::is_install_telemetry_enabled;

const OPENROUTER_HOST: &str = "openrouter.ai";
const NVIDIA_NIM_HOST: &str = "integrate.api.nvidia.com";
const CLOUDFLARE_API_HOST: &str = "api.cloudflare.com";
const CLOUDFLARE_AI_GATEWAY_HOST: &str = "gateway.ai.cloudflare.com";
const OPENCODE_HOST: &str = "opencode.ai";

fn host_of(base_url: &str) -> Option<String> {
    let without_scheme = base_url.split("://").nth(1).unwrap_or(base_url);
    let host = without_scheme.split(['/', '?', '#']).next().unwrap_or_default();
    let host = host.rsplit('@').next().unwrap_or(host);
    let host = host.split(':').next().unwrap_or(host);
    if host.is_empty() { None } else { Some(host.to_owned()) }
}

fn matches_host(base_url: &str, expected_host: &str) -> bool {
    host_of(base_url).as_deref() == Some(expected_host)
}

fn is_openrouter_model(model: &Model) -> bool {
    model.provider == "openrouter" || model.base_url.contains(OPENROUTER_HOST)
}

fn is_nvidia_nim_model(model: &Model) -> bool {
    model.provider == "nvidia" || matches_host(&model.base_url, NVIDIA_NIM_HOST)
}

fn is_cloudflare_model(model: &Model) -> bool {
    model.provider == "cloudflare-workers-ai"
        || model.provider == "cloudflare-ai-gateway"
        || matches_host(&model.base_url, CLOUDFLARE_API_HOST)
        || matches_host(&model.base_url, CLOUDFLARE_AI_GATEWAY_HOST)
}

fn default_attribution_headers(model: &Model, settings_manager: &SettingsManager) -> Option<BTreeMap<String, String>> {
    if !is_install_telemetry_enabled(settings_manager, None) {
        return None;
    }
    let mut headers = BTreeMap::new();
    if is_openrouter_model(model) {
        headers.insert("HTTP-Referer".to_owned(), "https://pi.dev".to_owned());
        headers.insert("X-OpenRouter-Title".to_owned(), app_name());
        headers.insert("X-OpenRouter-Categories".to_owned(), "cli-agent".to_owned());
        return Some(headers);
    }
    if is_nvidia_nim_model(model) {
        headers.insert("X-BILLING-INVOKE-ORIGIN".to_owned(), "Pi".to_owned());
        return Some(headers);
    }
    if is_cloudflare_model(model) {
        headers.insert("User-Agent".to_owned(), format!("{}-coding-agent", app_name()));
        return Some(headers);
    }
    None
}

fn session_headers(model: &Model, session_id: Option<&str>) -> Option<BTreeMap<String, String>> {
    let session_id = session_id?;
    if model.provider != "opencode" && model.provider != "opencode-go" && !matches_host(&model.base_url, OPENCODE_HOST) {
        return None;
    }
    let mut headers = BTreeMap::new();
    headers.insert("x-opencode-session".to_owned(), session_id.to_owned());
    headers.insert("x-opencode-client".to_owned(), "pi".to_owned());
    Some(headers)
}

pub fn merge_provider_attribution_headers(
    model: &Model,
    settings_manager: &SettingsManager,
    session_id: Option<&str>,
    header_sources: &[Option<BTreeMap<String, String>>],
) -> Option<BTreeMap<String, String>> {
    let mut merged = BTreeMap::new();
    if let Some(headers) = session_headers(model, session_id) {
        merged.extend(headers);
    }
    if let Some(headers) = default_attribution_headers(model, settings_manager) {
        merged.extend(headers);
    }
    for headers in header_sources.iter().flatten() {
        merged.extend(headers.clone());
    }
    if merged.is_empty() { None } else { Some(merged) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings_manager::{InMemorySettingsStorage, SettingsManager};
    use serde_json::json;

    fn model(provider: &str, base_url: &str) -> Model {
        serde_json::from_value(json!({
            "id": "m", "name": "M", "api": "openai-completions", "provider": provider, "baseUrl": base_url,
            "reasoning": false, "input": ["text"],
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
            "contextWindow": 1000, "maxTokens": 100
        }))
        .expect("model")
    }

    fn settings() -> SettingsManager {
        SettingsManager::from_storage(Box::new(InMemorySettingsStorage::default()), true)
    }

    #[test]
    fn the_host_parser_reads_the_hostname() {
        assert_eq!(host_of("https://openrouter.ai/api/v1").as_deref(), Some("openrouter.ai"));
        assert_eq!(host_of("https://user@host:8443/x").as_deref(), Some("host"));
    }

    #[test]
    fn an_openrouter_model_gets_attribution_headers() {
        let headers = merge_provider_attribution_headers(&model("openrouter", "https://x"), &settings(), None, &[]).expect("headers");
        assert_eq!(headers.get("X-OpenRouter-Title").map(String::as_str), Some("maho"));
    }

    #[test]
    fn an_opencode_session_adds_session_headers() {
        let headers = merge_provider_attribution_headers(&model("opencode", "https://x"), &settings(), Some("s1"), &[]).expect("headers");
        assert_eq!(headers.get("x-opencode-session").map(String::as_str), Some("s1"));
    }

    #[test]
    fn an_unattributed_model_has_no_headers() {
        assert!(merge_provider_attribution_headers(&model("anthropic", "https://x"), &settings(), None, &[]).is_none());
    }

    #[test]
    fn caller_headers_are_merged_last() {
        let mut extra = BTreeMap::new();
        extra.insert("X-Extra".to_owned(), "1".to_owned());
        let headers = merge_provider_attribution_headers(&model("openrouter", "https://x"), &settings(), None, &[Some(extra)]).expect("headers");
        assert_eq!(headers.get("X-Extra").map(String::as_str), Some("1"));
    }
}
