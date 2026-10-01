//! Request-time configuration key and header resolution.
use crate::model_config_schema::ModelsJsonProvider;
use crate::resolve_config_value::{resolve_config_value_or_throw, resolve_headers_or_throw};
use maho_ai::models::ProviderAuthResult;
use maho_ai::types::ProviderHeaders;
use std::collections::HashMap;

pub fn configured_api_key<'a>(config: Option<&'a ModelsJsonProvider>, extension: Option<&'a ModelsJsonProvider>) -> Option<&'a str> {
    extension.and_then(|c| c.api_key.as_deref()).or(config.and_then(|c| c.api_key.as_deref()))
}

pub fn configured_headers(config: Option<&ModelsJsonProvider>, extension: Option<&ModelsJsonProvider>) -> Option<HashMap<String, String>> {
    if config.and_then(|c| c.headers.as_ref()).is_none() && extension.and_then(|c| c.headers.as_ref()).is_none() { return None; }
    let mut headers: HashMap<String, String> = config.and_then(|c| c.headers.as_ref()).into_iter().flatten().map(|(k,v)| (k.clone(),v.clone())).collect();
    headers.extend(extension.and_then(|c| c.headers.as_ref()).into_iter().flatten().map(|(k,v)| (k.clone(),v.clone())));
    Some(headers)
}

pub fn configured_request_auth_status(config:Option<&ModelsJsonProvider>,extension:Option<&ModelsJsonProvider>)->Option<crate::provider_composer::AuthStatus> {
    use crate::resolve_config_value::{get_config_value_env_var_names,is_command_config_value,is_config_value_configured};
    use crate::provider_composer::AuthStatus;
    let key=configured_api_key(config,extension);
    let mut values:Vec<(String,&str)>=Vec::new();
    if let Some(key)=key {values.push((if extension.and_then(|c|c.api_key.as_ref()).is_some(){"fallback"}else{"models_json_key"}.into(),key));}
    let headers=configured_headers(config,extension);
    if key.is_none() {for (name,value) in headers.as_ref().into_iter().flatten(){if matches!(name.to_ascii_lowercase().as_str(),"authorization"|"x-api-key"|"api-key"|"cookie"){values.push((if extension.and_then(|c|c.headers.as_ref()).is_some_and(|h|h.contains_key(name)){"extension_headers"}else{"models_json_headers"}.into(),value));}}}
    if values.is_empty(){return None;}
    for (source,value) in values {
        if is_command_config_value(value){return Some(AuthStatus{configured:true,source:Some("models_json_command".into()),label:None});}
        let names=get_config_value_env_var_names(value);
        if !names.is_empty(){if is_config_value_configured(value,None){return Some(AuthStatus{configured:true,source:Some("environment".into()),label:Some(names.join(", "))});}continue;}
        if !value.trim().is_empty(){return Some(AuthStatus{configured:true,source:Some(source),label:None});}
    }
    Some(AuthStatus::default())
}

pub fn with_configured_auth(mut auth: ProviderAuthResult, headers: Option<ProviderHeaders>, auth_header: bool) -> Result<ProviderAuthResult, String> {
    if let Some(headers) = headers { auth.headers.get_or_insert_with(Default::default).extend(headers); }
    if auth_header {
        let key = auth.api_key.as_ref().filter(|k| !k.is_empty()).ok_or("authHeader requires a resolved API key")?;
        auth.headers.get_or_insert_with(Default::default).insert("Authorization".into(), Some(format!("Bearer {key}")));
    }
    Ok(auth)
}

pub async fn resolve_configured_auth(id: &str, config: Option<&ModelsJsonProvider>, extension: Option<&ModelsJsonProvider>, stored_key: Option<&str>, env: Option<&HashMap<String, String>>) -> Result<Option<ProviderAuthResult>, String> {
    let key = if let Some(key) = stored_key { Some(key.to_owned()) }
        else if let Some(raw) = configured_api_key(config, extension) { Some(resolve_config_value_or_throw(raw, &format!("API key for provider \"{id}\""), env).await?) }
        else {
            let provider_env = env.map(|v| v.iter().map(|(k,v)|(k.clone(),v.clone())).collect());
            maho_ai::env_api_keys::get_env_api_key(id, provider_env.as_ref())
        };
    let raw_headers = configured_headers(config, extension);
    let headers = resolve_headers_or_throw(raw_headers.as_ref(), &format!("provider \"{id}\""), env).await?
        .map(|v| v.into_iter().map(|(k,v)| (k,Some(v))).collect::<ProviderHeaders>());
    let credential_headers = headers.as_ref().is_some_and(|h| h.keys().any(|k| matches!(k.to_ascii_lowercase().as_str(), "authorization" | "x-api-key" | "api-key" | "cookie")));
    if key.is_none() && !credential_headers { return Ok(None); }
    let auth_header = extension.and_then(|c| c.auth_header).or(config.and_then(|c| c.auth_header)).unwrap_or(false);
    with_configured_auth(ProviderAuthResult { api_key:key, ..Default::default() }, headers, auth_header).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn stored_key_wins_over_unresolvable_configuration() {
        let config = ModelsJsonProvider {api_key:Some("${MISSING_KEY}".into()),auth_header:Some(true),..Default::default()};
        let auth = resolve_configured_auth("custom",Some(&config),None,Some("stored"),Some(&HashMap::new())).await.expect("resolve").expect("auth");
        assert_eq!(auth.api_key.as_deref(),Some("stored"));
        assert_eq!(auth.headers.expect("headers")["Authorization"].as_deref(),Some("Bearer stored"));
    }
    #[test]
    fn bearer_auth_requires_a_key() { assert!(with_configured_auth(Default::default(),None,true).is_err()); }
}
