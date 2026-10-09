use super::args::Args;
use super::auth_check::resolve_provider_for_print;
use super::auth_command::AuthCommandKind;
use maho_core::model_runtime::ModelRuntime;

const DEFAULT_BEARER_TOKEN_MIN_EXPIRY_MS: u64 = 30 * 60_000;

pub async fn resolve_credential_for_print(args: &Args, runtime: &ModelRuntime, kind: AuthCommandKind, min_expiry_ms: Option<u64>) -> Result<String, String> {
    let providers = resolve_provider_for_print(args, runtime).await?;
    let listed = runtime.list_credentials(None).await.map_err(|_| "Failed to resolve credential".to_owned())?;
    let mut values = Vec::new();
    for (provider, model) in &providers {
        let info = listed.iter().find(|entry| entry.provider_id == *provider);
        let is_oauth = info.is_some_and(|entry| entry.credential_type == maho_ai::auth::types::CredentialType::OAuth);
        if kind == AuthCommandKind::ApiKey && is_oauth || kind == AuthCommandKind::BearerToken && !is_oauth { continue; }
        let auth = if kind == AuthCommandKind::BearerToken {
            if let Some(model) = model { runtime.get_auth_for_model(model, &maho_ai::models::AuthResolutionOverrides { min_oauth_validity_ms: Some(min_expiry_ms.unwrap_or(DEFAULT_BEARER_TOKEN_MIN_EXPIRY_MS)), ..Default::default() }).await }
            else { runtime.get_auth_with_overrides(provider, &maho_ai::models::AuthResolutionOverrides { min_oauth_validity_ms: Some(min_expiry_ms.unwrap_or(DEFAULT_BEARER_TOKEN_MIN_EXPIRY_MS)), ..Default::default() }).await }
        } else if let Some(model) = model { runtime.get_auth_for_model(model, &Default::default()).await } else { runtime.get_auth(provider).await };
        if let Some(auth) = auth.map_err(|_| "Failed to resolve credential".to_owned())? {
            if let Some(value) = super::auth_command::get_auth_credential(Some(&auth)) { values.push((provider.clone(), value.to_owned())); }
        }
    }
    if values.len() == 1 { return Ok(values.remove(0).1); }
    if values.is_empty() { return Err(format!("No usable {} is configured", if kind == AuthCommandKind::ApiKey { "API key" } else { "OAuth bearer token" })); }
    Err(format!("Multiple configured providers matched ({}). Specify --provider.", values.iter().map(|(provider, _)| provider.as_str()).collect::<Vec<_>>().join(", ")))
}

#[cfg(test)]
mod tests {
    #[test]
    fn bearer_default_expiry_is_thirty_minutes() { assert_eq!(super::DEFAULT_BEARER_TOKEN_MIN_EXPIRY_MS, 1_800_000); }
}
