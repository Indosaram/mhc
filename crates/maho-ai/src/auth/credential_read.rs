//! Port of senpi packages/ai/src/auth/credential-read.ts.

use crate::auth::pool::slots::{PooledCredential, project_slot};
use crate::auth::resolve::{ModelsError, with_cause_detail};
use crate::auth::types::{AuthOperationOptions, Credential, CredentialStore};
use crate::utils::abort::AbortSignal;
use crate::utils::config_reference_error::{ConfigReferenceError, ConfigReferenceReason};
use serde_json::Value;

fn read_failed(provider_id: &str, cause: &str) -> ModelsError {
    ModelsError::new(
        "auth",
        with_cause_detail(
            &format!("Credential store read failed for {provider_id}"),
            Some(cause),
        ),
    )
}

fn options(signal: &AbortSignal) -> Option<AuthOperationOptions> {
    Some(AuthOperationOptions {
        signal: Some(signal.clone()),
    })
}

pub async fn read_credential(
    credentials: &dyn CredentialStore,
    provider_id: &str,
    signal: &AbortSignal,
) -> Result<Option<Credential>, ModelsError> {
    credentials
        .read(provider_id, options(signal))
        .await
        .map_err(|error| read_failed(provider_id, &error.to_string()))
}

/// The raw stored `key` of `slot_name` when it is still an object: the typed slot model cannot
/// hold one, so the fallback projection would silently drop the account instead of refusing it.
fn unresolved_slot_key(credential: &Credential, slot_name: &str) -> Option<Value> {
    let Credential::ApiKey(credential) = credential else {
        return None;
    };
    credential
        .extra
        .get("accounts")?
        .as_array()?
        .iter()
        .find(|slot| slot.get("name").and_then(Value::as_str) == Some(slot_name))?
        .get("key")
        .filter(|key| key.is_object())
        .cloned()
}

/// A key still holding an object was never resolved by its declaring store, so it never reaches a
/// provider.
pub async fn read_slot_credential(
    credentials: &dyn CredentialStore,
    provider_id: &str,
    slot_name: &str,
    signal: &AbortSignal,
) -> Result<Option<Credential>, ModelsError> {
    if credentials.has_read_slot() {
        return credentials
            .read_slot(provider_id, slot_name, options(signal))
            .await
            .map_err(|error| read_failed(provider_id, &error.to_string()));
    }
    let stored = read_credential(credentials, provider_id, signal).await?;
    if let Some(key) = stored
        .as_ref()
        .and_then(|stored| unresolved_slot_key(stored, slot_name))
    {
        let cause = ConfigReferenceError::new(
            format!("{provider_id}.accounts.{slot_name}.key"),
            key,
            ConfigReferenceReason::Forbidden,
        );
        return Err(read_failed(provider_id, &cause.to_string()));
    }
    Ok(stored
        .map(PooledCredential::from)
        .and_then(|pooled| project_slot(Some(&pooled), slot_name)))
}
