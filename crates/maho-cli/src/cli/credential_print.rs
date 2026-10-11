use super::args::Args;
use super::auth_check::resolve_provider_for_print;
use super::auth_command::AuthCommandKind;
use maho_core::model_runtime::ModelRuntime;

const DEFAULT_BEARER_TOKEN_MIN_EXPIRY_MS: u64 = 30 * 60_000;

pub async fn resolve_credential_for_print(args: &Args, runtime: &ModelRuntime, kind: AuthCommandKind, min_expiry_ms: Option<u64>) -> Result<String, String> {
    super::auth_command::validate_auth_command_args(args, kind)?;
    let listed = runtime.list_credentials(None).await.map_err(|_| "Failed to resolve credential".to_owned())?;
    let providers = resolve_provider_for_print(args, runtime, &listed)?;
    let mut values = Vec::new();
    for (provider, model) in &providers {
        let info = listed.iter().find(|entry| entry.provider_id == *provider);
        let is_oauth = info.is_some_and(|entry| entry.credential_type == maho_ai::auth::types::CredentialType::OAuth);
        if kind == AuthCommandKind::ApiKey && is_oauth || kind == AuthCommandKind::BearerToken && !is_oauth { continue; }
        let auth = if kind == AuthCommandKind::BearerToken {
            if let Some(model) = model { runtime.get_auth_for_model(model, &maho_ai::models::AuthResolutionOverrides { min_oauth_validity_ms: Some(min_expiry_ms.unwrap_or(DEFAULT_BEARER_TOKEN_MIN_EXPIRY_MS)), ..Default::default() }).await }
            else { runtime.get_auth_with_overrides(provider, &maho_ai::models::AuthResolutionOverrides { min_oauth_validity_ms: Some(min_expiry_ms.unwrap_or(DEFAULT_BEARER_TOKEN_MIN_EXPIRY_MS)), ..Default::default() }).await }
        } else if let Some(model) = model { runtime.get_auth_for_model(model, &Default::default()).await } else { runtime.get_auth(provider).await };
        if let Some(auth) = auth.map_err(|_| "Failed to resolve credential".to_owned())?
            && let Some(value) = super::auth_command::get_auth_credential(Some(&auth))
        {
            values.push((provider.clone(), value.to_owned()));
        }
    }
    if values.len() == 1 { return Ok(values.remove(0).1); }
    if values.is_empty() {
        if args.provider.is_some() && let Some((provider, _)) = providers.first() {
            let is_oauth = listed.iter().any(|entry| entry.provider_id == *provider && entry.credential_type == maho_ai::auth::types::CredentialType::OAuth);
            if kind == AuthCommandKind::ApiKey && is_oauth { return Err(format!("Provider \"{provider}\" is configured with OAuth, not an API key")); }
            if kind == AuthCommandKind::BearerToken && !is_oauth { return Err(format!("Provider \"{provider}\" is not configured with an OAuth bearer token")); }
        }
        return Err(format!("No usable {} is configured", if kind == AuthCommandKind::ApiKey { "API key" } else { "OAuth bearer token" }));
    }
    Err(format!("Multiple configured providers matched ({}). Specify --provider.", values.iter().map(|(provider, _)| provider.as_str()).collect::<Vec<_>>().join(", ")))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bearer_default_expiry_is_thirty_minutes() { assert_eq!(super::DEFAULT_BEARER_TOKEN_MIN_EXPIRY_MS, 1_800_000); }

    #[tokio::test]
    async fn api_key_print_returns_stored_key() {
        let runtime = ModelRuntime::create_sync(Default::default());
        runtime.credentials.set("openai", Some(serde_json::json!({"type":"api_key","key":"fixture-key"}))).expect("seed");
        let args = Args { provider: Some("openai".into()), ..Default::default() };
        assert_eq!(resolve_credential_for_print(&args, &runtime, AuthCommandKind::ApiKey, None).await.expect("key"), "fixture-key");
    }

    #[tokio::test]
    async fn bearer_print_extracts_header_token_without_unneeded_refresh() {
        let (runtime, calls) = super::super::auth_check::tests::oauth_fixture();
        runtime.credentials.set("chatgpt-subscription", Some(serde_json::json!({"type":"oauth","access":"header-token","refresh":"fixture","expires":f64::MAX}))).expect("seed");
        let args = Args { provider: Some("chatgpt-subscription".into()), ..Default::default() };
        assert_eq!(resolve_credential_for_print(&args, &runtime, AuthCommandKind::BearerToken, None).await.expect("token"), "header-token");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn bearer_print_refreshes_and_persists_expired_token() {
        let (runtime, calls) = super::super::auth_check::tests::oauth_fixture();
        let args = Args { provider: Some("chatgpt-subscription".into()), ..Default::default() };
        assert_eq!(resolve_credential_for_print(&args, &runtime, AuthCommandKind::BearerToken, None).await.expect("token"), "fresh");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(runtime.credentials.get("chatgpt-subscription").expect("stored")["access"], "fresh");
    }

    #[tokio::test]
    async fn api_key_print_rejects_oauth_with_provider_specific_error() {
        let storage = std::sync::Arc::new(maho_core::auth_storage::AuthStorage::in_memory(Default::default()));
        storage.set("chatgpt-subscription", Some(serde_json::json!({"type":"oauth", "access":"fixture", "refresh":"fixture", "expires":0}))).expect("stored");
        let runtime = ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions { credentials: Some(storage), ..Default::default() });
        let args = Args { provider: Some("chatgpt-subscription".into()), ..Default::default() };
        let error = resolve_credential_for_print(&args, &runtime, AuthCommandKind::ApiKey, None).await.expect_err("wrong type");
        assert_eq!(error, "Provider \"chatgpt-subscription\" is configured with OAuth, not an API key");
    }

    #[tokio::test]
    async fn bearer_print_rejects_api_key_with_provider_specific_error() {
        let storage = std::sync::Arc::new(maho_core::auth_storage::AuthStorage::in_memory(Default::default()));
        storage.set("xai", Some(serde_json::json!({"type":"api_key", "key":"fixture"}))).expect("stored");
        let runtime = ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions { credentials: Some(storage), ..Default::default() });
        let args = Args { provider: Some("xai".into()), ..Default::default() };
        let error = resolve_credential_for_print(&args, &runtime, AuthCommandKind::BearerToken, None).await.expect_err("wrong type");
        assert_eq!(error, "Provider \"xai\" is not configured with an OAuth bearer token");
    }
}
