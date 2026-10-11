use super::args::Args;
use super::auth_command::{get_auth_credential, AuthCommandKind};
use maho_core::model_runtime::ModelRuntime;
use serde::Serialize;

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthCheckStatus { Ready, NotReady, Invalid }

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthCheckReason { ProviderNotFound, CredentialsNotConfigured, CredentialNotAvailable, InvalidState }

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct AuthCheckResult {
    pub status: AuthCheckStatus,
    pub provider: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<AuthCheckReason>,
    #[serde(rename = "authType", skip_serializing_if = "Option::is_none")]
    pub auth_type: Option<String>,
}

fn resolve_provider(args: &Args, runtime: &ModelRuntime) -> Result<String, String> {
    let (provider, model) = super::auth_command::validate_auth_command_args(args, AuthCommandKind::Check)?;
    if let Some(model) = model {
        let resolved = maho_core::model_resolver::resolve_cli_model(provider, Some(model), None, runtime);
        if let Some(error) = resolved.error { return Err(error); }
        return resolved.parsed.model.map(|model| model.provider)
            .ok_or_else(|| format!("Unable to resolve model \"{model}\""));
    }
    provider.map(str::to_owned).ok_or_else(|| "Unable to resolve an auth provider".to_owned())
}

pub(crate) fn resolve_provider_for_print(args: &Args, runtime: &ModelRuntime, listed: &[maho_ai::auth::types::CredentialInfo]) -> Result<Vec<(String, Option<maho_ai::types::Model>)>, String> {
    if let Some(provider) = args.provider.as_deref().filter(|value| !value.trim().is_empty()) {
        if runtime.get_provider(provider).is_none() { return Err(format!("Unknown provider \"{provider}\". Use --list-models to see available providers.")); }
        let model = if let Some(model) = args.model.as_deref() {
            let resolved = maho_core::model_resolver::resolve_cli_model(Some(provider), Some(model), None, runtime);
            if let Some(error) = resolved.error { return Err(error); }
            Some(resolved.parsed.model.ok_or("Unable to resolve the requested provider/model")?)
        } else { None };
        return Ok(vec![(provider.trim().to_owned(), model)]);
    }
    if let Some(model) = args.model.as_deref() {
        let mut providers = Vec::new();
        for provider in runtime.get_providers() {
            if !listed.iter().any(|entry| entry.provider_id == provider.id()) { continue; }
            let resolved = maho_core::model_resolver::resolve_cli_model(Some(provider.id()), Some(model), None, runtime);
            if resolved.error.is_none() && !resolved.parsed.warning.as_deref().is_some_and(|warning| warning.contains("Using custom model id"))
                && let Some(model) = resolved.parsed.model
            { providers.push((provider.id().to_owned(), Some(model))); }
        }
        if providers.is_empty() { return Err(format!("Model \"{model}\" not found. Use --list-models to see available models.")); }
        return Ok(providers);
    }
    Err("Credential printing requires --provider <provider> or --model <model>".to_owned())
}

pub async fn check_provider_auth(args: &Args, runtime: &ModelRuntime, refresh: bool) -> Result<AuthCheckResult, String> {
    let provider = resolve_provider(args, runtime)?;
    if runtime.get_error().is_some() || !runtime.credentials.errors().is_empty() { return Ok(AuthCheckResult { status: AuthCheckStatus::Invalid, provider, reason: Some(AuthCheckReason::InvalidState), auth_type: None }); }
    if runtime.get_provider(&provider).is_none() { return Ok(AuthCheckResult { status: AuthCheckStatus::NotReady, provider, reason: Some(AuthCheckReason::ProviderNotFound), auth_type: None }); }
    match runtime.check_auth(&provider, None).await {
        Ok(Some(auth)) => {
            if refresh && runtime.get_auth(&provider).await.map_err(|_| "invalid auth state".to_owned())?.is_none() {
                return Ok(AuthCheckResult { status: AuthCheckStatus::NotReady, provider, reason: Some(AuthCheckReason::CredentialsNotConfigured), auth_type: None });
            }
            let auth_type = match auth.auth_type { maho_ai::auth::types::AuthType::ApiKey => "api_key", maho_ai::auth::types::AuthType::OAuth => "oauth" };
            Ok(AuthCheckResult { status: AuthCheckStatus::Ready, provider, reason: None, auth_type: Some(auth_type.to_owned()) })
        }
        Ok(None) => Ok(AuthCheckResult { status: AuthCheckStatus::NotReady, provider, reason: Some(AuthCheckReason::CredentialsNotConfigured), auth_type: None }),
        Err(_) => Ok(AuthCheckResult { status: AuthCheckStatus::Invalid, provider, reason: Some(AuthCheckReason::InvalidState), auth_type: None }),
    }
}

pub async fn provider_credential(runtime: &ModelRuntime, provider: &str, refresh: bool) -> Result<Option<String>, String> {
    if !refresh
        && let Some(value) = runtime.credentials.get(provider)
        && maho_core::auth_storage::credential_kind(&value) == Some(maho_core::auth_storage::CredentialKind::Oauth)
    {
        return Ok(value.get("access").and_then(serde_json::Value::as_str).map(str::to_owned));
    }
    Ok(get_auth_credential(runtime.get_auth(provider).await.map_err(|_| "invalid auth state".to_owned())?.as_ref()).map(str::to_owned))
}

pub fn requested_provider(args: &Args) -> String { args.provider.clone().or_else(|| args.model.clone()).unwrap_or_default() }

#[allow(dead_code)]
fn _kind_is_check(kind: AuthCommandKind) -> bool { kind == AuthCommandKind::Check }

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    struct FixtureOAuth(std::sync::Arc<std::sync::atomic::AtomicUsize>);
    #[async_trait::async_trait]
    impl maho_ai::auth::types::OAuthAuth for FixtureOAuth {
        fn name(&self) -> &str { "auth-fixture" }
        async fn login(&self, _: &maho_ai::auth::types::ProviderAuthInteraction) -> anyhow::Result<maho_ai::auth::types::OAuthCredential> {
            Ok(maho_ai::auth::types::OAuthCredential::new("old", "refresh", 0.0))
        }
        async fn refresh(&self, _: &maho_ai::auth::types::OAuthCredential, _: &maho_ai::utils::abort::AbortSignal) -> anyhow::Result<maho_ai::auth::types::OAuthCredential> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(maho_ai::auth::types::OAuthCredential::new("fresh", "refresh", f64::MAX))
        }
        async fn to_auth(&self, credential: &maho_ai::auth::types::OAuthCredential) -> anyhow::Result<maho_ai::auth::types::ModelAuth> {
            Ok(maho_ai::auth::types::ModelAuth { headers: Some(std::collections::BTreeMap::from([("Authorization".into(), Some(format!("Bearer {}", credential.access)))])), ..Default::default() })
        }
        async fn check(&self, _: &dyn maho_ai::auth::types::AuthContext, credential: Option<&maho_ai::auth::types::OAuthCredential>, _: &maho_ai::utils::abort::AbortSignal) -> anyhow::Result<Option<maho_ai::auth::types::AuthCheck>> {
            Ok(credential.map(|_| maho_ai::auth::types::AuthCheck { source: Some("stored".into()), auth_type: maho_ai::auth::types::AuthType::OAuth }))
        }
    }

    pub(in crate::cli) fn oauth_fixture() -> (ModelRuntime, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut runtime = ModelRuntime::create_sync(Default::default());
        runtime.register_provider("chatgpt-subscription", maho_core::provider_composer::ProviderConfigInput {
            oauth: Some(std::sync::Arc::new(FixtureOAuth(calls.clone()))), ..Default::default()
        }).expect("provider");
        runtime.credentials.set("chatgpt-subscription", Some(serde_json::json!({
            "type":"oauth", "access":"old", "refresh":"refresh", "expires":0
        }))).expect("seed");
        (runtime, calls)
    }

    #[tokio::test]
    async fn configured_provider_check_is_ready() {
        let runtime = ModelRuntime::create_sync(Default::default());
        runtime.credentials.set("openai", Some(serde_json::json!({"type":"api_key", "key":"fixture"}))).expect("seed");
        let result = check_provider_auth(&Args { provider: Some("openai".into()), ..Default::default() }, &runtime, false).await.expect("check");
        assert_eq!(result.status, AuthCheckStatus::Ready);
        assert_eq!(result.auth_type.as_deref(), Some("api_key"));
    }

    #[tokio::test]
    async fn unknown_provider_check_is_not_ready() {
        let runtime = ModelRuntime::create_sync(Default::default());
        let result = check_provider_auth(&Args { provider: Some("not-installed".into()), ..Default::default() }, &runtime, false).await.expect("check");
        assert_eq!(result.status, AuthCheckStatus::NotReady);
        assert_eq!(result.reason, Some(AuthCheckReason::ProviderNotFound));
    }

    #[tokio::test]
    async fn no_refresh_reads_expired_oauth_without_exchange() {
        let (runtime, calls) = oauth_fixture();
        assert_eq!(provider_credential(&runtime, "chatgpt-subscription", false).await.expect("credential").as_deref(), Some("old"));
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn builtin_oauth_without_check_reports_stored_credential_ready() {
        let runtime = ModelRuntime::create_sync(Default::default());
        runtime.credentials.set("chatgpt-subscription", Some(serde_json::json!({
            "type":"oauth", "access":"old", "refresh":"refresh", "expires":0
        }))).expect("seed");
        let result = check_provider_auth(&Args { provider: Some("chatgpt-subscription".into()), ..Default::default() }, &runtime, false).await.expect("check");
        assert_eq!(result.status, AuthCheckStatus::Ready);
        assert_eq!(result.auth_type.as_deref(), Some("oauth"));
    }

    #[tokio::test]
    async fn refresh_check_exchanges_expired_oauth_once() {
        let (runtime, calls) = oauth_fixture();
        let result = check_provider_auth(&Args { provider: Some("chatgpt-subscription".into()), ..Default::default() }, &runtime, true).await.expect("check");
        assert_eq!(result.status, AuthCheckStatus::Ready);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(runtime.credentials.get("chatgpt-subscription").expect("credential")["access"], "fresh");
    }

    #[tokio::test]
    async fn malformed_persisted_auth_is_invalid() {
        let root = tempfile::tempdir().expect("temp");
        let path = root.path().join("auth.json");
        std::fs::write(&path, "{invalid-json").expect("fixture");
        let runtime = ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions { auth_path: Some(path), ..Default::default() });
        let result = check_provider_auth(&Args { provider: Some("openai".into()), ..Default::default() }, &runtime, false).await.expect("check");
        assert_eq!(result.status, AuthCheckStatus::Invalid);
        assert_eq!(result.reason, Some(AuthCheckReason::InvalidState));
    }

    #[tokio::test]
    async fn unresolved_stored_environment_key_is_not_ready() {
        let runtime = ModelRuntime::create_sync(Default::default());
        runtime.credentials.set("openai", Some(serde_json::json!({"type":"api_key", "key":"$MISSING_AUTH_CHECK_FIXTURE_KEY_9237"}))).expect("seed");
        let result = check_provider_auth(&Args { provider: Some("openai".into()), ..Default::default() }, &runtime, false).await.expect("check");
        assert_eq!(result.status, AuthCheckStatus::NotReady);
        assert_eq!(result.reason, Some(AuthCheckReason::CredentialsNotConfigured));
    }
    #[test]
    fn status_serializes_to_cli_names() {
        assert_eq!(serde_json::to_string(&AuthCheckStatus::NotReady).unwrap(), "\"not_ready\"");
        assert_eq!(serde_json::to_string(&AuthCheckReason::CredentialNotAvailable).unwrap(), "\"credential_not_available\"");
    }
    #[test]
    fn requested_provider_prefers_provider() {
        let args = Args { provider: Some("openai".to_owned()), model: Some("openai/gpt".to_owned()), ..Default::default() };
        assert_eq!(requested_provider(&args), "openai");
    }

    #[test]
    fn auth_model_only_resolves_unqualified_model() {
        let runtime = ModelRuntime::create_sync(Default::default());
        let model = runtime.get_models(Some("xai")).into_iter().find(|model|
            runtime.get_models(None).iter().filter(|candidate| candidate.id == model.id).count() == 1).expect("unique builtin model");
        let args = Args { model: Some(model.id), ..Default::default() };
        assert_eq!(resolve_provider(&args, &runtime).expect("resolved"), "xai");
    }

    #[test]
    fn auth_provider_model_uses_custom_model_resolution() {
        let runtime = ModelRuntime::create_sync(Default::default());
        let args = Args { provider: Some("xai".into()), model: Some("fixture-custom-id".into()), ..Default::default() };
        assert_eq!(resolve_provider(&args, &runtime).expect("custom model"), "xai");
    }

    #[test]
    fn credential_provider_model_keeps_resolved_model() {
        let runtime = ModelRuntime::create_sync(Default::default());
        let args = Args { provider: Some("xai".into()), model: Some("fixture-custom-id".into()), ..Default::default() };
        let rows = resolve_provider_for_print(&args, &runtime, &[]).expect("resolved");
        assert_eq!(rows[0].1.as_ref().expect("model").id, "fixture-custom-id");
    }

    #[test]
    fn credential_model_only_searches_configured_providers() {
        let runtime = ModelRuntime::create_sync(Default::default());
        let model = runtime.get_models(Some("xai")).into_iter().next().expect("model");
        let args = Args { model: Some(model.id.clone()), ..Default::default() };
        let listed = [maho_ai::auth::types::CredentialInfo { provider_id: "xai".into(), credential_type: maho_ai::auth::types::CredentialType::ApiKey }];
        let rows = resolve_provider_for_print(&args, &runtime, &listed).expect("configured match");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, "xai");
        assert_eq!(rows[0].1.as_ref().expect("model").id, model.id);
        assert!(resolve_provider_for_print(&args, &runtime, &[]).is_err());
    }

    #[test]
    fn credential_model_only_excludes_custom_model_candidates() {
        let runtime = ModelRuntime::create_sync(Default::default());
        let args = Args { model: Some("fixture-nonexistent-id".into()), ..Default::default() };
        let listed = [maho_ai::auth::types::CredentialInfo { provider_id: "xai".into(), credential_type: maho_ai::auth::types::CredentialType::ApiKey }];
        assert!(resolve_provider_for_print(&args, &runtime, &listed).is_err());
    }

}
