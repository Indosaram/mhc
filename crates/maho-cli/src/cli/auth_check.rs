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
    if let Some(provider) = args.provider.as_deref().filter(|value| !value.trim().is_empty()) {
        if let Some(model) = args.model.as_deref().filter(|value| !value.trim().is_empty()) {
            let Some(resolved) = runtime.get_model(provider, model).or_else(|| runtime.get_model(provider, &format!("{provider}/{model}"))) else {
                return Err(format!("Unable to resolve model \"{model}\""));
            };
            return Ok(resolved.provider);
        }
        return Ok(provider.trim().to_owned());
    }
    let model = args.model.as_deref().ok_or("Unable to resolve an auth provider")?;
    let (provider, model_id) = model.split_once('/').ok_or_else(|| format!("Unable to resolve model \"{model}\""))?;
    if runtime.get_model(provider, model_id).is_none() { return Err(format!("Unable to resolve model \"{model}\"")); }
    Ok(provider.to_owned())
}

pub(crate) async fn resolve_provider_for_print(args: &Args, runtime: &ModelRuntime) -> Result<Vec<(String, Option<maho_ai::types::Model>)>, String> {
    if let Some(provider) = args.provider.as_deref().filter(|value| !value.trim().is_empty()) {
        if runtime.get_provider(provider).is_none() { return Err(format!("Unknown provider \"{provider}\". Use --list-models to see available providers.")); }
        return Ok(vec![(provider.trim().to_owned(), None)]);
    }
    if let Some(model) = args.model.as_deref() {
        let (provider, model_id) = model.split_once('/').ok_or_else(|| format!("Model \"{model}\" not found. Use --list-models to see available models."))?;
        if runtime.get_model(provider, model_id).is_none() { return Err(format!("Model \"{model}\" not found. Use --list-models to see available models.")); }
        return Ok(vec![(provider.to_owned(), runtime.get_model(provider, model_id))]);
    }
    Err("Credential printing requires --provider <provider> or --model <model>".to_owned())
}

pub async fn check_provider_auth(args: &Args, runtime: &ModelRuntime, refresh: bool) -> Result<AuthCheckResult, String> {
    let provider = resolve_provider(args, runtime)?;
    if runtime.get_error().is_some() { return Ok(AuthCheckResult { status: AuthCheckStatus::Invalid, provider, reason: Some(AuthCheckReason::InvalidState), auth_type: None }); }
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
    if !refresh {
        if let Some(value) = runtime.credentials.get(provider)
            && maho_core::auth_storage::credential_kind(&value) == Some(maho_core::auth_storage::CredentialKind::Oauth)
        {
            return Ok(value.get("access").and_then(serde_json::Value::as_str).map(str::to_owned));
        }
    }
    Ok(get_auth_credential(runtime.get_auth(provider).await.map_err(|_| "invalid auth state".to_owned())?.as_ref()).map(str::to_owned))
}

pub fn requested_provider(args: &Args) -> String { args.provider.clone().or_else(|| args.model.clone()).unwrap_or_default() }

#[allow(dead_code)]
fn _kind_is_check(kind: AuthCommandKind) -> bool { kind == AuthCommandKind::Check }

#[cfg(test)]
mod tests {
    use super::*;
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
}
