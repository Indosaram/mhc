use super::registry::{JsonRpcError, MethodRegistration, MethodRegistry, MethodScope};
use maho_core::{auth_storage::AuthStorage, credential_accounts::{get_credential_accounts, pin_credential_account, remove_credential_account}, credential_pool::state_store::CredentialSlotRepository};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};

fn provider(params: &Value, method: &str) -> Result<String, JsonRpcError> {
    if !params.is_object() { return Err(JsonRpcError::new(-32600, format!("{method} params must be an object"))); }
    let provider = params["provider"].as_str().filter(|provider| !provider.is_empty()).ok_or_else(|| JsonRpcError::new(-32600, format!("{method} provider must be a non-empty string")))?;
    Ok(maho_ai::legacy_provider_ids::normalize_provider_id(provider))
}
pub fn register_account_methods(registry: &mut MethodRegistry, agent_dir: String) {
    for method in ["account/read", "account/providerAccounts/read", "account/providerAccounts/pin", "account/providerAccounts/remove", "account/rateLimits/read", "account/usage/read"] {
        let directory = agent_dir.clone();
        registry.register(method.into(), MethodRegistration { requires_init:true, experimental:false, scope:MethodScope::Global,
            handler:Arc::new(move |context| { let directory = directory.clone(); Box::pin(async move {
                tokio::task::spawn_blocking(move || tokio::runtime::Handle::current().block_on(async move {
                let params = &context.request["params"];
                if method == "account/rateLimits/read" { return Err(JsonRpcError::new(-32600, "codex account authentication required to read rate limits")); }
                if method == "account/usage/read" { return Err(JsonRpcError::new(-32600, "codex account authentication required to read token usage")); }
                let path = Path::new(&directory).join("auth.json").to_string_lossy().into_owned();
                let storage = AuthStorage::create(&path);
                if method == "account/read" {
                    if !params.is_null() && !params.is_object() { return Err(JsonRpcError::new(-32600, "account/read params must be an object")); }
                    if params.get("refreshToken").is_some_and(|value| !value.is_boolean()) { return Err(JsonRpcError::new(-32600, "account/read refreshToken must be a boolean")); }
                    return Ok(json!({"account":if storage.list().is_empty() { Value::Null } else { json!({"type":"apiKey"}) },"requiresOpenaiAuth":false}));
                }
                let provider = provider(params, method)?;
                let repository = CredentialSlotRepository::new(&Path::new(&directory).join("credential-pool-state.json").to_string_lossy());
                let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|error| JsonRpcError::new(-32603, error.to_string()))?.as_millis();
                let now = u64::try_from(now).map_err(|error| JsonRpcError::new(-32603, error.to_string()))?;
                let env = |key: &str| std::env::var(key).ok();
                match method {
                    "account/providerAccounts/read" => {
                        let accounts = get_credential_accounts(&storage, &provider, &env, &repository, now).await.map_err(|error| JsonRpcError::new(-32603, error))?;
                        let accounts = accounts.iter().map(|account| {
                            let mut wire = json!({"name":account.name,"source":account.source.as_str(),"blocked":account.blocked,"pinned":account.pinned});
                            if let Some(name) = &account.display_name { wire["displayName"] = json!(name); } wire
                        }).collect::<Vec<_>>();
                        Ok(json!({"provider":provider,"accounts":accounts}))
                    },
                    "account/providerAccounts/pin" => {
                        let name = match params.get("name") { Some(Value::Null) => None, Some(Value::String(name)) => Some(name.as_str()), _ => return Err(JsonRpcError::new(-32600, "account/providerAccounts/pin name must be a string or null")) };
                        pin_credential_account(&storage, &provider, name, &env, &repository, now).await.map_err(|error| JsonRpcError::new(-32603, error))?;
                        Ok(json!({}))
                    },
                    "account/providerAccounts/remove" => {
                        let name = params["name"].as_str().filter(|name| !name.is_empty()).ok_or_else(|| JsonRpcError::new(-32600, "account/providerAccounts/remove name must be a non-empty string"))?;
                        remove_credential_account(&storage, &provider, name, &env, &repository, now).await.map_err(|error| JsonRpcError::new(-32603, error))?;
                        Ok(json!({}))
                    },
                    _ => unreachable!(),
                }
                })).await.map_err(|error| JsonRpcError::new(-32603, error.to_string()))?
            }) }) });
    }
}
pub fn provider_account_event_notification(event: &Value) -> Value {
    if event["type"] == "accounts_changed" { json!({"method":"account/providerAccounts/updated","params":{"provider":event["provider"]}}) }
    else { json!({"method":"account/providerAccounts/failover","params":{"provider":event["provider"],"from":event["from"],"to":event["to"],"reason":event["reason"]}}) }
}
/// Native port of the pinned `providerAccountEventNotification`, consuming the typed event the
/// core provider-account registry delivers (no JSON round-trip).
pub fn provider_account_event_notification_native(event: &maho_core::ProviderAccountEvent) -> Value {
    match event {
        maho_core::ProviderAccountEvent::AccountsChanged { provider } => json!({"method":"account/providerAccounts/updated","params":{"provider":provider}}),
        maho_core::ProviderAccountEvent::Failover { provider, from, to, reason } => json!({"method":"account/providerAccounts/failover","params":{"provider":provider,"from":from,"to":to,"reason":reason}}),
    }
}
