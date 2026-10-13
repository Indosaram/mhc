//! Port of senpi packages/coding-agent/src/core/remote-catalog-provider.ts.
//!
//! `with_remote_catalog` wraps a static builtin provider with a persisted pi.dev catalog overlay:
//! the stored overlay is restored on every refresh and revalidated over the network when allowed
//! and stale.

use maho_ai::models_store::ModelsStoreEntry;
use maho_ai::model::Model;

use crate::remote_catalog_merge::{RemoteCatalogConflict, merge_remote_catalog_models};

pub const DEFAULT_CATALOG_BASE_URL: &str = "https://pi.dev";
pub const REMOTE_CATALOG_ATTEMPT_TIMEOUT_MS: u64 = 4_000;
pub const REMOTE_CATALOG_REFRESH_INTERVAL_MS: i64 = 4 * 60 * 60 * 1000;

/// Builtin providers that exist only in this fork; the upstream pi.dev catalog does not serve them.
pub const FORK_ONLY_BUILTIN_PROVIDERS: [&str; 2] = ["alibaba-token-plan", "opengateway"];

/// Whether the remote catalog overlay can serve a provider. Fork-only providers are skipped under
/// the default upstream catalog base URL; a custom base URL may serve them.
pub fn remote_catalog_serves_provider(provider_id: &str, catalog_base_url: Option<&str>) -> bool {
    catalog_base_url.is_some() || !FORK_ONLY_BUILTIN_PROVIDERS.contains(&provider_id)
}

/// The stored overlay models a refresh may publish, honoring the local generated-at cutoff.
pub fn remote_models(entry: Option<&ModelsStoreEntry>, local_generated_at: Option<i64>) -> Vec<Model> {
    let Some(entry) = entry else { return Vec::new() };
    if let Some(local) = local_generated_at
        && (entry.last_modified.is_none() || entry.last_modified.unwrap_or(0) <= local)
    {
        return Vec::new();
    }
    entry.models.clone()
}

/// Whether a refresh must hit the network: forced, unchecked, or past the freshness window.
pub fn should_refresh(
    force: bool,
    stored: Option<&ModelsStoreEntry>,
    now_ms: i64,
) -> bool {
    if force {
        return true;
    }
    match (stored.and_then(|entry| entry.checked_at), stored.and_then(|entry| entry.last_modified)) {
        (Some(checked_at), Some(_)) => now_ms - checked_at >= REMOTE_CATALOG_REFRESH_INTERVAL_MS,
        _ => true,
    }
}

/// The persisted overlay of one provider: the models it contributes and the capability conflicts
/// its merge produced against the static baseline.
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteCatalogOverlay {
    provider_id: String,
    dynamic_models: Vec<Model>,
    conflicts: Vec<RemoteCatalogConflict>,
}

impl RemoteCatalogOverlay {
    pub fn new(provider_id: &str) -> Self {
        Self { provider_id: provider_id.to_owned(), dynamic_models: Vec::new(), conflicts: Vec::new() }
    }

    pub fn conflicts(&self) -> &[RemoteCatalogConflict] {
        &self.conflicts
    }

    /// Applies an overlay: sets the dynamic models and recomputes conflicts against the baseline.
    pub fn apply(&mut self, baseline: &[Model], dynamic: Vec<Model>) {
        self.conflicts = merge_remote_catalog_models(&self.provider_id, baseline, &dynamic).conflicts;
        self.dynamic_models = dynamic;
    }

    /// The provider's models with the overlay applied.
    pub fn merged_models(&self, baseline: &[Model]) -> Vec<Model> {
        merge_remote_catalog_models(&self.provider_id, baseline, &self.dynamic_models).models
    }
}

const REMOTE_CATALOG_MAX_RETRIES: u32 = 2;

/// senpi `withRemoteCatalog`: the wrapped provider's models are its static catalog merged with the
/// overlay; everything else (auth, streams) is the base provider's.
pub fn with_remote_catalog(
    provider: std::sync::Arc<dyn maho_ai::models::Provider>,
    catalog_base_url: Option<&str>,
    local_generated_at: Option<i64>,
) -> std::sync::Arc<dyn maho_ai::models::Provider> {
    std::sync::Arc::new(RemoteCatalogProvider {
        base: provider,
        catalog_base_url: catalog_base_url.unwrap_or(DEFAULT_CATALOG_BASE_URL).trim_end_matches('/').to_owned(),
        local_generated_at,
        dynamic_models: std::sync::Arc::new(std::sync::RwLock::new(Vec::new())),
    })
}

struct RemoteCatalogProvider {
    base: std::sync::Arc<dyn maho_ai::models::Provider>,
    catalog_base_url: String,
    local_generated_at: Option<i64>,
    dynamic_models: std::sync::Arc<std::sync::RwLock<Vec<Model>>>,
}

enum CatalogResponse {
    NotModified,
    Missing,
    Failed(u16),
    Fresh { models: Vec<Model>, last_modified: i64, etag: Option<String> },
}

fn encode_component(value: &str) -> String {
    value.bytes().map(|byte| match byte {
        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')' => (byte as char).to_string(),
        _ => format!("%{byte:02X}"),
    }).collect()
}

impl RemoteCatalogProvider {
    fn publish_update(&self, models: Vec<Model>) -> Box<dyn FnOnce() + Send> {
        let dynamic = self.dynamic_models.clone();
        Box::new(move || *dynamic.write().unwrap_or_else(|p| p.into_inner()) = models)
    }

    /// senpi `fetchWithRetry(url, …, { attemptTimeoutMs })`: bounded attempts, retrying transport
    /// errors and 5xx/429 responses.
    async fn fetch(&self, validator: Option<&str>, signal: &maho_ai::utils::abort::AbortSignal) -> Result<CatalogResponse, String> {
        let url = format!("{}/api/models/providers/{}", self.catalog_base_url, encode_component(self.base.id()));
        let client = reqwest::Client::new();
        let mut last_error = String::new();
        for attempt in 0..=REMOTE_CATALOG_MAX_RETRIES {
            signal.throw_if_aborted().map_err(|reason| reason.message)?;
            let mut request = client.get(&url).header("accept", "application/json")
                .header("User-Agent", maho_ai::utils::pi_user_agent::get_pi_user_agent())
                .timeout(std::time::Duration::from_millis(REMOTE_CATALOG_ATTEMPT_TIMEOUT_MS));
            if let Some(validator) = validator { request = request.header("if-none-match", validator); }
            let response = match maho_ai::utils::abort::race_with_abort_signal(request.send(), signal).await.map_err(|reason| reason.message)? {
                Ok(response) => response,
                Err(error) => { last_error = error.to_string(); continue; }
            };
            let status = response.status().as_u16();
            if (status == 429 || status >= 500) && status != 501 && attempt < REMOTE_CATALOG_MAX_RETRIES { last_error = format!("HTTP {status}"); continue; }
            if status == 304 { return Ok(CatalogResponse::NotModified); }
            if status == 404 || status == 501 { return Ok(CatalogResponse::Missing); }
            if !response.status().is_success() { return Ok(CatalogResponse::Failed(status)); }
            let header = |name: &str| response.headers().get(name).and_then(|value| value.to_str().ok()).map(str::to_owned);
            let last_modified = header("last-modified").and_then(|value| chrono::DateTime::parse_from_rfc2822(&value).ok()).map_or(0, |value| value.timestamp_millis());
            let etag = header("etag");
            let body: serde_json::Value = response.json().await.map_err(|error| error.to_string())?;
            let models = crate::remote_catalog_merge::parse_remote_catalog(self.base.id(), &body)?;
            return Ok(CatalogResponse::Fresh { models, last_modified, etag });
        }
        Err(last_error)
    }
}

impl maho_ai::models::Provider for RemoteCatalogProvider {
    fn id(&self) -> &str { self.base.id() }
    fn name(&self) -> &str { self.base.name() }
    fn base_url(&self) -> Option<&str> { self.base.base_url() }
    fn headers(&self) -> Option<&maho_ai::types::ProviderHeaders> { self.base.headers() }
    fn auth(&self) -> Option<&maho_ai::auth::types::ProviderAuth> { self.base.auth() }
    fn retry_policy(&self) -> Option<&maho_ai::utils::retry_profile::types::RetryPolicyProfile> { self.base.retry_policy() }
    fn get_models(&self) -> Vec<Model> {
        merge_remote_catalog_models(self.base.id(), &self.base.get_models(), &self.dynamic_models.read().unwrap_or_else(|p| p.into_inner())).models
    }
    fn supports_refresh(&self) -> bool { true }
    fn refresh_models<'a>(&'a self, context: maho_ai::models::RefreshModelsContext) -> maho_ai::types::BoxFuture<'a, Result<(), maho_ai::models::ModelsError>> {
        use maho_ai::models::{ModelsError, ModelsErrorCode, ModelsPublication};
        Box::pin(async move {
            let stored = context.stored.clone();
            let restored: Vec<Model> = remote_models(stored.as_ref(), self.local_generated_at).into_iter().filter(|model| model.provider == self.base.id()).collect();
            if !context.publish(ModelsPublication { persist: None, update: Some(self.publish_update(restored)) }).await.unwrap_or(false) { return Ok(()); }
            if !context.allow_network || context.signal.aborted() { return Ok(()); }
            if !should_refresh(context.force.unwrap_or(false), stored.as_ref(), maho_ai::utils::diagnostics::now_ms()) { return Ok(()); }
            // Only revalidate when a cached body backs the validator, so a 304 never empties the overlay.
            let validator = stored.as_ref().filter(|entry| !entry.models.is_empty()).and_then(|entry| entry.etag.clone());
            let response = self.fetch(validator.as_deref(), &context.signal).await;
            if context.signal.aborted() { return Ok(()); }
            let checked_at = maho_ai::utils::diagnostics::now_ms();
            let base = stored.clone().unwrap_or_default();
            match response.map_err(|error| ModelsError::new(ModelsErrorCode::ModelSource, format!("Model catalog request failed for {}: {error}", self.base.id())))? {
                CatalogResponse::NotModified if stored.is_some() => {
                    let _ = context.publish(ModelsPublication { persist: Some(Some(ModelsStoreEntry { checked_at: Some(checked_at), ..base })), update: None }).await;
                }
                CatalogResponse::Missing | CatalogResponse::NotModified => {
                    let _ = context.publish(ModelsPublication { persist: Some(Some(ModelsStoreEntry { checked_at: Some(checked_at), last_modified: Some(0), etag: None, ..base })), update: None }).await;
                }
                CatalogResponse::Failed(status) => {
                    let _ = context.publish(ModelsPublication { persist: Some(Some(ModelsStoreEntry { checked_at: Some(checked_at), ..base })), update: None }).await;
                    return Err(ModelsError::new(ModelsErrorCode::ModelSource, format!("Model catalog request failed for {}: {status}", self.base.id())));
                }
                CatalogResponse::Fresh { models, last_modified, etag } => {
                    let entry = ModelsStoreEntry { models, checked_at: Some(checked_at), last_modified: Some(last_modified), etag };
                    let published = remote_models(Some(&entry), self.local_generated_at);
                    let _ = context.publish(ModelsPublication { persist: Some(Some(entry)), update: Some(self.publish_update(published)) }).await;
                }
            }
            Ok(())
        })
    }
    fn filter_models(&self, models: Vec<Model>, credential: Option<&maho_ai::models::Credential>) -> Vec<Model> { self.base.filter_models(models, credential) }
    fn stream(&self, model: &Model, context: &maho_ai::types::Context, options: Option<maho_ai::types::StreamOptions>) -> maho_ai::types::AssistantMessageEventStream {
        self.base.stream(model, context, options)
    }
    fn stream_simple(&self, model: &Model, context: &maho_ai::types::Context, options: Option<maho_ai::types::SimpleStreamOptions>) -> maho_ai::types::AssistantMessageEventStream {
        self.base.stream_simple(model, context, options)
    }
    fn fetch_deferred(&self, model: &Model, handle: &maho_ai::types::DeferredHandle, options: Option<maho_ai::types::DeferredFetchOptions>) -> Option<maho_ai::types::AssistantMessageEventStream> {
        self.base.fetch_deferred(model, handle, options)
    }
    fn cancel_deferred<'a>(&'a self, model: &'a Model, handle: &'a maho_ai::types::DeferredHandle, options: Option<maho_ai::types::DeferredCancelOptions>) -> maho_ai::types::BoxFuture<'a, Result<(), String>> {
        self.base.cancel_deferred(model, handle, options)
    }
    fn supports_cancel_deferred(&self) -> bool { self.base.supports_cancel_deferred() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(id: &str, context_window: u64) -> Model {
        serde_json::from_value(serde_json::json!({
            "id": id, "name": "M", "api": "openai-completions", "provider": "p", "baseUrl": "",
            "reasoning": false, "input": ["text"],
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
            "contextWindow": context_window, "maxTokens": 100
        }))
        .expect("model")
    }

    #[test]
    fn fork_only_providers_need_a_custom_base_url() {
        assert!(!remote_catalog_serves_provider("opengateway", None));
        assert!(remote_catalog_serves_provider("opengateway", Some("https://fork.example")));
        assert!(remote_catalog_serves_provider("anthropic", None));
    }

    #[test]
    fn the_overlay_skips_models_older_than_the_local_generated_at() {
        let entry = ModelsStoreEntry { models: vec![model("a", 1000)], last_modified: Some(100), ..Default::default() };
        assert!(remote_models(Some(&entry), Some(200)).is_empty());
        assert_eq!(remote_models(Some(&entry), Some(50)).len(), 1);
        assert_eq!(remote_models(Some(&entry), None).len(), 1);
    }

    #[test]
    fn a_missing_entry_has_no_overlay_models() {
        assert!(remote_models(None, None).is_empty());
    }

    #[test]
    fn the_freshness_window_gates_refresh() {
        let stored = ModelsStoreEntry { models: vec![model("a", 1000)], checked_at: Some(0), last_modified: Some(1), ..Default::default() };
        assert!(!should_refresh(false, Some(&stored), REMOTE_CATALOG_REFRESH_INTERVAL_MS - 1));
        assert!(should_refresh(false, Some(&stored), REMOTE_CATALOG_REFRESH_INTERVAL_MS));
        assert!(should_refresh(true, Some(&stored), 0));
        assert!(should_refresh(false, None, 0));
    }

    #[test]
    fn applying_an_overlay_merges_and_records_conflicts() {
        let mut overlay = RemoteCatalogOverlay::new("p");
        overlay.apply(&[model("a", 1000)], vec![model("a", 5000)]);
        assert_eq!(overlay.conflicts().len(), 1);
        assert_eq!(overlay.merged_models(&[model("a", 1000)])[0].context_window, 1000);
    }
}
