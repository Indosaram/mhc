use crate::contract::{CatalogModel, CatalogSource, LoadedCatalog, ProviderEnv};
use maho_ext_api::ProviderModelConfig;

#[derive(Clone, Debug, Default)]
pub struct RuntimeStatus {
    pub transport: Option<String>,
    pub source: Option<CatalogSource>,
    pub model_count: usize,
    pub last_success: Option<String>,
    pub last_attempt: Option<String>,
    pub cache_path: String,
    pub endpoint: String,
    pub refresh_in_flight: bool,
    pub warning: Option<String>,
}

pub fn format_status(status: &RuntimeStatus) -> String {
    let transport = status.transport.as_deref().unwrap_or("unknown");
    let source = status
        .source
        .as_ref()
        .map(CatalogSource::as_str)
        .unwrap_or("none");
    let last_success = status.last_success.as_deref().unwrap_or("never");
    let last_attempt = status.last_attempt.as_deref().unwrap_or("never");
    let refresh = if status.refresh_in_flight {
        "in progress"
    } else {
        "idle"
    };
    let warning = status.warning.as_deref().unwrap_or("none");
    format!(
        "transport: {transport}\nsource: {source}\nmodels: {}\nlast success: {last_success}\nlast attempt: {last_attempt}\ncache: {}\nendpoint: {}\nrefresh: {refresh}\nwarning: {warning}",
        status.model_count, status.cache_path, status.endpoint
    )
}

pub fn warning_prefix(message: &str) -> String {
    format!("[commandcode] {message}")
}

pub fn refresh_result_warning(loaded: &LoadedCatalog) -> Option<String> {
    if loaded.models.is_empty() {
        return Some(
            loaded
                .warning
                .clone()
                .unwrap_or_else(|| "Model catalog refresh returned no models".to_owned()),
        );
    }
    loaded.warning.clone()
}

pub fn status(env: &ProviderEnv) -> RuntimeStatus {
    let loaded = crate::models::load_models_sync(env);
    RuntimeStatus {
        transport: None,
        source: Some(loaded.source),
        model_count: loaded.models.len(),
        last_success: None,
        last_attempt: Some(
            chrono::Utc::now()
                .format("%Y-%m-%d %H:%M:%S UTC")
                .to_string(),
        ),
        cache_path: env.cache_path.display().to_string(),
        endpoint: env.models_url.clone(),
        refresh_in_flight: false,
        warning: loaded.warning,
    }
}

pub async fn refresh(env: &ProviderEnv) -> LoadedCatalog {
    crate::models::load_models(env, None).await
}

/// True when the cached catalog is missing, unreadable, or older than `MODEL_CACHE_MAX_AGE`.
fn cache_is_stale(path: &std::path::Path) -> bool {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .map(|modified| {
            modified
                .elapsed()
                .map(|age| age > crate::contract::MODEL_CACHE_MAX_AGE)
                .unwrap_or(true)
        })
        .unwrap_or(true)
}

pub fn discover_models(env: &ProviderEnv) -> Vec<ProviderModelConfig> {
    // senpi registers with whatever the catalog resolves to at startup. Native `register` is
    // synchronous, so awaiting a live fetch here would block the whole extension load on the
    // network (bounded only by `DEFAULT_MODELS_TIMEOUT_MS`, 10s). The cached catalog is equally
    // valid for registration, so serve it without touching the network and refresh behind the
    // callback; `/commandcode-refresh` still forces a live reload on demand.
    let cached = crate::models::read_cache(&env.cache_path, &env.api_base).ok();
    if let Some(models) = cached {
        if cache_is_stale(&env.cache_path) {
            let refresh_env = env.clone();
            std::thread::spawn(move || {
                if let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    let _ = runtime.block_on(crate::models::load_models(&refresh_env, None));
                }
            });
        }
        return models
            .into_iter()
            .map(CatalogModel::into_provider_config)
            .collect();
    }
    crate::models::load_models_sync(env)
        .models
        .into_iter()
        .map(CatalogModel::into_provider_config)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_cache_is_stale() {
        let dir = std::env::temp_dir().join(format!("cc-stale-missing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        assert!(cache_is_stale(&dir.join("absent.json")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn freshly_written_cache_is_not_stale() {
        let dir = std::env::temp_dir().join(format!("cc-stale-fresh-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("catalog.json");
        std::fs::write(&path, "{}").expect("write cache");
        assert!(!cache_is_stale(&path));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
