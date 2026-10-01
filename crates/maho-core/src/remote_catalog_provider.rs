//! Port of senpi packages/coding-agent/src/core/remote-catalog-provider.ts (the policy half).
//!
//! senpi wraps a builtin provider to add a persisted remote-catalog overlay, fetching through its
//! management HTTP helper. In maho, provider construction and the fetch transport live in maho-ai
//! (CreateProviderOptions.fetch_models / restore_models), so this module owns the freshness policy,
//! the fork-only provider set and the overlay/conflict bookkeeping; the request itself is issued by
//! maho-ai's transport.

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
