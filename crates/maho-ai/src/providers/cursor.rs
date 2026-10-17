//! Port of senpi packages/ai/src/providers/cursor.ts.

use crate::api::cursor_agent::FetchCursorUsableModelsOptions;
use crate::api::cursor_agent_lazy::fetch_cursor_usable_models;
use crate::cursor::catalog_grouping::{CursorCatalogEntry, CursorCatalogRawEntry, normalize_cursor_catalog};
use crate::cursor::context_limit_store::resolve_cursor_context_window;
use crate::cursor::store_migration::regroup_stored_cursor_models;
use crate::model::{CursorAgentCompat, CursorReasoning, ModelCompat};
use crate::models::{
    CreateProviderOptions, Credential, FetchModels, ModelsError, ModelsErrorCode, Provider, ProviderApi,
    RefreshModelsContext, RestoreModels, create_provider,
};
use crate::auth::types::ProviderAuth;
use crate::auth::helpers::lazy_oauth;
use crate::providers::builtin_api_streams;
use crate::types::{InputModality, Model, ModelCost};
use crate::utils::abort::AbortSignal;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;

pub const CURSOR_BASE_URL: &str = "https://api2.cursor.sh";

/// `credential?.type === "oauth" ? credential.access : credential?.key`.
fn credential_access_token(credential: Option<&Credential>) -> Option<String> {
    let credential = credential?;
    let field = if credential.get("type").and_then(Value::as_str) == Some("oauth") { "access" } else { "key" };
    credential.get(field).and_then(Value::as_str).filter(|token| !token.is_empty()).map(str::to_owned)
}

fn to_modality(value: &str) -> Option<InputModality> {
    match value {
        "text" => Some(InputModality::Text),
        "image" => Some(InputModality::Image),
        "video" => Some(InputModality::Video),
        _ => None,
    }
}

fn to_cursor_model(entry: CursorCatalogEntry, max_tokens_by_variant: &BTreeMap<String, u64>) -> Model {
    let representative =
        entry.representative_variant_id.clone().or_else(|| entry.legacy_aliases.first().cloned()).unwrap_or_else(|| entry.id.clone());
    let max_tokens =
        max_tokens_by_variant.get(&representative).or_else(|| max_tokens_by_variant.get(&entry.id)).copied().unwrap_or(64_000);
    let cursor_reasoning = match (&entry.capability_id, &entry.representative_variant_id) {
        (Some(capability_id), Some(representative_variant_id)) => Some(CursorReasoning {
            capability_id: capability_id.clone(),
            thinking_mode: entry.thinking_mode,
            representative_variant_id: representative_variant_id.clone(),
            variant_ids: entry.variant_ids.clone(),
        }),
        _ => None,
    };
    let compat = CursorAgentCompat { cursor_max_mode: entry.cursor_max_mode.then_some(true), cursor_reasoning };
    let compat = match serde_json::to_value(&compat) {
        Ok(Value::Object(map)) => ModelCompat(map),
        _ => ModelCompat::default(),
    };
    Model {
        upstream_model_id: entry.representative_variant_id.clone().filter(|id| id != &entry.id),
        context_window: resolve_cursor_context_window(&entry.id, entry.window) as u64,
        input: entry.input.iter().filter_map(|value| to_modality(value)).collect(),
        id: entry.id,
        name: entry.name,
        api: "cursor-agent".to_owned(),
        provider: "cursor".to_owned(),
        base_url: CURSOR_BASE_URL.to_owned(),
        reasoning: entry.reasoning,
        default_thinking_level: None,
        thinking_level_map: entry.thinking_level_map,
        // Subscription-billed: Cursor reports no per-token pricing here.
        cost: ModelCost::default(),
        max_tokens,
        sampling_params: None,
        headers: None,
        cache_retention: None,
        service_tier: None,
        recover_text_tool_calls: None,
        compat: Some(compat),
        input_limits: None,
    }
}

/// Cursor's catalog is fully dynamic: `GetUsableModels` requires an authenticated access token and
/// reports the account's usable models, so the provider ships no static baseline.
async fn fetch_cursor_models(credential: Option<Credential>, signal: AbortSignal) -> Result<Vec<Model>, ModelsError> {
    let Some(access_token) = credential_access_token(credential.as_ref()) else { return Ok(Vec::new()) };
    let discovered = fetch_cursor_usable_models(FetchCursorUsableModelsOptions {
        api_key: access_token,
        base_url: None,
        timeout_ms: None,
        signal: Some(signal),
    })
    .await
    .ok_or_else(|| {
        ModelsError::new(ModelsErrorCode::ModelSource, "Could not load Cursor model catalog from GetUsableModels")
    })?;
    let raw: Vec<CursorCatalogRawEntry> = discovered
        .iter()
        .map(|model| CursorCatalogRawEntry {
            id: model.id.clone(),
            name: model.name.clone(),
            input: model.input.clone(),
            cursor_max_mode: model.cursor_max_mode,
        })
        .collect();
    let max_tokens_by_variant: BTreeMap<String, u64> =
        discovered.iter().map(|model| (model.id.clone(), model.max_tokens)).collect();
    Ok(normalize_cursor_catalog(&raw).into_iter().map(|entry| to_cursor_model(entry, &max_tokens_by_variant)).collect())
}

/// Cursor subscription provider (OAuth only). The model catalog is discovered per account through
/// `GetUsableModels` after login (`Models.refresh` runs automatically after a successful
/// `/login cursor`).
pub fn cursor_provider() -> Arc<dyn Provider> {
    let restore: RestoreModels = Arc::new(|models| Ok(regroup_stored_cursor_models(&models)));
    let fetch: FetchModels = Arc::new(|context: &RefreshModelsContext| {
        Box::pin(fetch_cursor_models(context.credential.clone(), context.signal.clone()))
    });
    create_provider(CreateProviderOptions {
        id: "cursor".into(),
        name: Some("Cursor".into()),
        base_url: Some(CURSOR_BASE_URL.to_owned()),
        headers: None,
        auth: Some(ProviderAuth { api_key: None, oauth: Some(Arc::new(lazy_oauth("Cursor (Pro/Ultra/Teams)", true, Some("Sign in with Cursor".into()), || Box::pin(crate::auth::oauth::load::load_cursor_oauth())))) }),
        models: Vec::new(),
        fetch_models: Some(fetch),
        restore_models: Some(restore),
        filter_models: None,
        api: ProviderApi::Single(builtin_api_streams("cursor-agent")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{
        AuthResolution, AuthResolutionOverrides, CreateModelsOptions, ModelsAuth, ModelsRefreshOptions, create_models,
    };
    use crate::models_store::{InMemoryModelsStore, ModelsStore, ModelsStoreEntry};
    use crate::types::BoxFuture;

    #[derive(serde::Deserialize)]
    struct FixtureEntry {
        id: String,
        name: String,
        input: Vec<String>,
    }

    struct NoCredentialAuth;

    impl ModelsAuth for NoCredentialAuth {
        fn resolve<'a>(
            &'a self,
            _provider: &'a dyn Provider,
            _overrides: &'a AuthResolutionOverrides,
        ) -> BoxFuture<'a, Result<Option<AuthResolution>, ModelsError>> {
            Box::pin(async { Ok(None) })
        }

        fn refresh_credential<'a>(
            &'a self,
            _provider: &'a dyn Provider,
            _stored: Option<&'a Credential>,
            _signal: &'a AbortSignal,
        ) -> BoxFuture<'a, Result<Option<Credential>, ModelsError>> {
            Box::pin(async { Ok(None) })
        }
    }

    fn legacy_model(entry: &FixtureEntry) -> Model {
        Model {
            id: entry.id.clone(),
            name: entry.name.clone(),
            api: "cursor-agent".to_owned(),
            provider: "cursor".to_owned(),
            base_url: CURSOR_BASE_URL.to_owned(),
            reasoning: false,
            default_thinking_level: None,
            thinking_level_map: None,
            input: entry.input.iter().filter_map(|value| to_modality(value)).collect(),
            cost: ModelCost::default(),
            context_window: 200_000,
            max_tokens: 64_000,
            sampling_params: None,
            headers: None,
            cache_retention: None,
            upstream_model_id: None,
            service_tier: None,
            recover_text_tool_calls: None,
            compat: None,
            input_limits: None,
        }
    }

    #[test]
    fn refreshes_its_catalog_through_get_usable_models() {
        assert!(cursor_provider().supports_refresh());
    }

    #[test]
    fn reads_the_access_token_from_oauth_access_or_api_key() {
        let oauth = serde_json::json!({ "type": "oauth", "access": "tok", "refresh": "r" });
        let api_key = serde_json::json!({ "type": "api_key", "key": "abc" });
        assert_eq!(credential_access_token(Some(&oauth)).as_deref(), Some("tok"));
        assert_eq!(credential_access_token(Some(&api_key)).as_deref(), Some("abc"));
        assert_eq!(credential_access_token(None), None);
    }

    #[tokio::test]
    async fn regroups_a_stored_flat_catalog_on_restore() {
        let fixture: Vec<FixtureEntry> =
            serde_json::from_str(include_str!("../../tests/fixtures/cursor-usable-models-20260818.json")).expect("fixture");
        let legacy: Vec<Model> = fixture.iter().map(legacy_model).collect();
        let expected = regroup_stored_cursor_models(&legacy);
        let store: Arc<dyn ModelsStore> = Arc::new(InMemoryModelsStore::new());
        store
            .write("cursor", &ModelsStoreEntry { models: legacy.clone(), ..ModelsStoreEntry::default() }, None)
            .await
            .expect("seed store");
        let models =
            create_models(Some(CreateModelsOptions { models_store: Some(store), auth: Some(Arc::new(NoCredentialAuth)) }));
        models.set_provider(cursor_provider());

        let result =
            models.refresh(ModelsRefreshOptions { allow_network: Some(false), ..ModelsRefreshOptions::default() }).await;
        assert!(result.errors.is_empty());
        let restored = models.get_models(Some("cursor"));
        assert!(restored.len() < legacy.len());
        assert_eq!(
            restored.iter().map(|model| model.id.clone()).collect::<Vec<_>>(),
            expected.iter().map(|model| model.id.clone()).collect::<Vec<_>>()
        );
    }
}
