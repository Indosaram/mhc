//! Port of senpi packages/coding-agent/src/core/auth-credential-values.ts.
//!
//! auth.json is trusted user configuration: an api-key `key` (flat or `accounts[i].key`) may be a
//! configuration reference that resolves per use against the auth.json that declares it. Credentials are
//! handled as raw JSON here because the typed `ApiKeyCredential.key` is a string: a reference must be
//! resolved before the credential is decoded.

use std::collections::HashMap;
use std::path::Path;

use maho_ai::auth::pool::slots::{PooledCredential, project_slot};
use maho_ai::auth::types::Credential;
use maho_ai::utils::config_reference::{is_config_reference, parse_config_reference};
use maho_ai::utils::config_reference_error::{ConfigReferenceError, ConfigReferenceReason};
use serde_json::Value;

use crate::config_reference_resolver::{
    ConfigReferenceContext, ConfigValue, resolve_config_reference,
};
use crate::resolve_config_value::resolve_config_value;

fn assert_key_shape(value: &Value, field: &str) -> Result<(), ConfigReferenceError> {
    if value.is_object() && !is_config_reference(value) {
        return Err(ConfigReferenceError::new(
            field,
            value.clone(),
            ConfigReferenceReason::Malformed,
        ));
    }
    Ok(())
}

/// Every flat and pooled `key` object must be exactly a reference shape.
pub fn assert_credential_references(
    provider_id: &str,
    credential: &Value,
) -> Result<(), ConfigReferenceError> {
    if credential.get("type").and_then(Value::as_str) == Some("api_key")
        && let Some(key) = credential.get("key")
    {
        assert_key_shape(key, &format!("{provider_id}.key"))?;
    }
    for (index, account) in credential
        .get("accounts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        if let Some(key) = account.get("key") {
            assert_key_shape(key, &format!("{provider_id}.accounts.{index}.key"))?;
        }
    }
    Ok(())
}

/// Whether an api-key `key` value is acceptable on load: absent, a string, or a reference.
pub fn is_valid_stored_key(key: Option<&Value>) -> bool {
    match key {
        None | Some(Value::String(_)) => true,
        Some(other) => is_config_reference(other),
    }
}

fn credential_env(credential: &Value) -> Option<HashMap<String, String>> {
    let env = credential.get("env")?.as_object()?;
    Some(
        env.iter()
            .filter_map(|(key, value)| value.as_str().map(|value| (key.clone(), value.to_owned())))
            .collect(),
    )
}

/// auth.json is trusted user configuration, so a reference resolves against the file that declares it.
fn resolve_stored_reference(
    reference: &Value,
    field: &str,
    auth_path: Option<&str>,
    env: Option<HashMap<String, String>>,
) -> Result<String, ConfigReferenceError> {
    let Some(parsed) = parse_config_reference(reference) else {
        return Err(ConfigReferenceError::new(
            field,
            reference.clone(),
            ConfigReferenceReason::Malformed,
        ));
    };
    let value = ConfigValue::Reference {
        reference: parsed,
        origin: None,
    };
    let context = auth_path.map(|path| ConfigReferenceContext {
        origin: path.to_owned(),
        base_dir: Path::new(path).parent().map(Path::to_path_buf),
        authority: "trusted".into(),
        field: field.to_owned(),
        env: None,
    });
    // `{ ...process.env, ...credential.env }` when the credential carries env, else the process env.
    let env = env.map(|overlay| {
        let mut merged: HashMap<String, String> = std::env::vars().collect();
        merged.extend(overlay);
        merged
    });
    resolve_config_reference(&value, context.as_ref(), env.as_ref())
}

/// senpi `resolveStoredCredential`: the flat key of an api-key credential resolves (reference, command or
/// template); other credentials pass through. The returned JSON decodes as a typed `Credential`.
pub async fn resolve_stored_credential(
    credential: Value,
    provider_id: &str,
    auth_path: Option<&str>,
) -> Result<Value, ConfigReferenceError> {
    if credential.get("type").and_then(Value::as_str) != Some("api_key") {
        return Ok(credential);
    }
    assert_credential_references(provider_id, &credential)?;
    let Some(key) = credential.get("key").cloned() else {
        return Ok(credential);
    };
    let mut credential = credential;
    let env = credential_env(&credential);
    match key {
        Value::String(text) => match resolve_config_value(&text, env.as_ref()).await {
            Some(resolved) => credential["key"] = Value::String(resolved),
            None => {
                if let Some(object) = credential.as_object_mut() {
                    object.remove("key");
                }
            }
        },
        reference => {
            let resolved = resolve_stored_reference(
                &reference,
                &format!("{provider_id}.key"),
                auth_path,
                env,
            )?;
            credential["key"] = Value::String(resolved);
        }
    }
    Ok(credential)
}

/// senpi `resolveReadOnlyCredential`: like `resolve_stored_credential`, except a string key resolves only
/// when it is not a command (a read-only store never runs commands it did not already run).
pub async fn resolve_read_only_credential(
    credential: Value,
    provider_id: &str,
    auth_path: &str,
) -> Result<Value, ConfigReferenceError> {
    let is_api_key = credential.get("type").and_then(Value::as_str) == Some("api_key");
    match credential.get("key") {
        Some(key) if is_api_key && is_config_reference(key) => {
            let key = key.clone();
            let env = credential_env(&credential);
            let resolved = resolve_stored_reference(
                &key,
                &format!("{provider_id}.key"),
                Some(auth_path),
                env,
            )?;
            let mut credential = credential;
            credential["key"] = Value::String(resolved);
            Ok(credential)
        }
        Some(Value::String(key))
            if is_api_key
                && !key.is_empty()
                && !crate::resolve_config_value::is_command_config_value(key) =>
        {
            let env = credential_env(&credential);
            let resolved = resolve_config_value(key, env.as_ref()).await;
            let mut credential = credential;
            match resolved {
                Some(resolved) => credential["key"] = Value::String(resolved),
                None => {
                    if let Some(object) = credential.as_object_mut() {
                        object.remove("key");
                    }
                }
            }
            Ok(credential)
        }
        _ => Ok(credential),
    }
}

fn decode(credential: Value) -> Result<Credential, String> {
    serde_json::from_value(credential)
        .map_err(|error| format!("Invalid auth.json credential: {error}"))
}

/// senpi `resolveStoredSlot`: a slot-scoped read resolves only the selected slot's reference, never the flat
/// key or a sibling. A flat credential is its one `default` slot, read exactly as `read_flat` reads it.
pub async fn resolve_stored_slot<F, Fut>(
    credential: Option<Value>,
    provider_id: &str,
    slot_name: &str,
    auth_path: Option<&str>,
    read_flat: F,
) -> Result<Option<Credential>, String>
where
    F: FnOnce(Value) -> Fut,
    Fut: std::future::Future<Output = Result<Value, ConfigReferenceError>>,
{
    let Some(credential) = credential else {
        return Ok(None);
    };
    assert_credential_references(provider_id, &credential).map_err(|error| error.to_string())?;
    let accounts: Vec<Value> = credential
        .get("accounts")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if accounts.is_empty() {
        let flat = read_flat(credential)
            .await
            .map_err(|error| error.to_string())?;
        let pooled = PooledCredential::from(decode(flat)?);
        return Ok(project_slot(Some(&pooled), slot_name));
    }
    let index = accounts
        .iter()
        .position(|account| account.get("name").and_then(Value::as_str) == Some(slot_name));
    // Resolve only the selected slot's reference; siblings stay unread (and are dropped from the copy so the
    // typed pool can decode it).
    let mut copy = credential.clone();
    if let Some(object) = copy.as_object_mut() {
        if object.get("key").is_some_and(Value::is_object) {
            object.remove("key");
        }
        let selected: Vec<Value> = accounts
            .iter()
            .enumerate()
            .filter(|(position, account)| {
                Some(*position) == index || !account.get("key").is_some_and(Value::is_object)
            })
            .map(|(_, account)| account.clone())
            .collect();
        object.insert("accounts".into(), Value::Array(selected));
    }
    if let Some(index) = index
        && let Some(reference) = accounts[index].get("key").filter(|key| key.is_object())
    {
        let env = accounts[index]
            .get("env")
            .and_then(Value::as_object)
            .map(|env| {
                env.iter()
                    .filter_map(|(key, value)| {
                        value.as_str().map(|value| (key.clone(), value.to_owned()))
                    })
                    .collect()
            })
            .or_else(|| credential_env(&credential));
        let resolved = resolve_stored_reference(
            reference,
            &format!("{provider_id}.accounts.{index}.key"),
            auth_path,
            env,
        )
        .map_err(|error| error.to_string())?;
        if let Some(account) = copy
            .get_mut("accounts")
            .and_then(Value::as_array_mut)
            .and_then(|accounts| {
                accounts
                    .iter_mut()
                    .find(|account| account.get("name").and_then(Value::as_str) == Some(slot_name))
            })
        {
            account["key"] = Value::String(resolved);
        }
    }
    let pooled = PooledCredential::from(decode(copy)?);
    Ok(project_slot(Some(&pooled), slot_name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// senpi test/suite/regressions/3858-reference-auth.test.ts.
    #[tokio::test]
    async fn a_flat_env_reference_key_resolves_per_use() {
        let env_name = "MAHO_AUTH_REFERENCE_TEST_KEY";
        let credential = json!({"type": "api_key", "key": {"$env": env_name}, "env": {env_name: "from-credential-env"}});
        let resolved = resolve_stored_credential(credential, "p", Some("/cfg/auth.json"))
            .await
            .expect("resolved");
        assert_eq!(resolved["key"], "from-credential-env");
    }

    #[tokio::test]
    async fn malformed_key_objects_are_refused() {
        let credential = json!({"type": "api_key", "key": {"$cmd": "cat"}});
        let error = resolve_stored_credential(credential, "p", None)
            .await
            .expect_err("malformed");
        assert_eq!(error.reason, ConfigReferenceReason::Malformed);
        assert_eq!(error.field, "p.key");
        assert!(!is_valid_stored_key(Some(&json!({"$cmd": "cat"}))));
        assert!(is_valid_stored_key(Some(&json!({"$env": "X"}))));
    }

    #[tokio::test]
    async fn a_slot_read_resolves_only_the_selected_slot() {
        let credential = json!({
            "type": "api_key",
            "accounts": [
                {"name": "work", "key": {"$env": "MAHO_SLOT_WORK"}, "env": {"MAHO_SLOT_WORK": "work-key"}},
                {"name": "home", "key": {"$env": "MAHO_SLOT_UNSET_SIBLING"}}
            ]
        });
        let read = resolve_stored_slot(
            Some(credential),
            "p",
            "work",
            Some("/cfg/auth.json"),
            |flat| async move { Ok(flat) },
        )
        .await
        .expect("slot read");
        let Some(Credential::ApiKey(projected)) = read else {
            panic!("api key slot")
        };
        assert_eq!(projected.key.as_deref(), Some("work-key"));
    }
}
