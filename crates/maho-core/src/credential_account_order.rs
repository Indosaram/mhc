//! Port of senpi packages/coding-agent/src/core/credential-account-order.ts.

use serde_json::Value;

use crate::auth_storage::AuthStorage;
use crate::credential_accounts::{
    CredentialAccountSummary, assert_valid_account_name, get_credential_accounts,
    summarize_credential_accounts,
};
use crate::credential_pool::state_store::CredentialSlotRepository;
use crate::provider_account_events::emit_provider_accounts_changed;

/// A provider's accounts in effective order, and whether a user order shapes that order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialAccountListing {
    pub accounts: Vec<CredentialAccountSummary>,
    pub ordered: bool,
}

fn stored_order(credential: Option<&Value>) -> Vec<String> {
    credential
        .and_then(|credential| credential.get("order"))
        .and_then(Value::as_array)
        .map(|order| {
            order
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

pub async fn list_credential_accounts(
    storage: &AuthStorage,
    provider: &str,
    env: &(dyn Fn(&str) -> Option<String> + Sync),
    repository: &CredentialSlotRepository,
    now: u64,
) -> Result<CredentialAccountListing, String> {
    let accounts = get_credential_accounts(storage, provider, env, repository, now).await?;
    let order = stored_order(storage.get(provider).as_ref());
    let ordered = accounts.iter().any(|account| order.contains(&account.name));
    Ok(CredentialAccountListing { accounts, ordered })
}

fn assert_order_names(names: &[String], known: &[CredentialAccountSummary]) -> Result<(), String> {
    if names.is_empty() {
        return Err("Account order must name at least one account".to_owned());
    }
    let mut seen: Vec<&str> = Vec::new();
    for name in names {
        assert_valid_account_name(name)?;
        if seen.contains(&name.as_str()) {
            return Err(format!("Duplicate provider account in order: {name}"));
        }
        seen.push(name);
        if !known.iter().any(|account| &account.name == name) {
            return Err(format!("Provider account not found: {name}"));
        }
    }
    Ok(())
}

fn with_order(mut credential: Value, names: Option<&[String]>) -> Value {
    if let Some(object) = credential.as_object_mut() {
        object.shift_remove("order");
        if let Some(names) = names {
            object.insert("order".to_owned(), Value::from(names.to_vec()));
        }
    }
    credential
}

/// Sets the provider's account order, or clears it when `names` is `None` (senpi#3591). The order lives
/// beside the pin in the stored credential, so it has the pin's scope: a provider with nothing stored has
/// no order to keep. The names are validated against the credential this write replaces, under the
/// store's lock, so an account removed concurrently cannot leave its name in the order.
pub async fn order_credential_accounts(
    storage: &AuthStorage,
    provider: &str,
    names: Option<&[String]>,
    env: &(dyn Fn(&str) -> Option<String> + Sync),
    repository: &CredentialSlotRepository,
    now: u64,
) -> Result<(), String> {
    storage
        .modify_credential(provider, |current| async move {
            let Some(current) = current else {
                return Err(format!("No stored credential for provider: {provider}"));
            };
            if let Some(names) = names {
                let known =
                    summarize_credential_accounts(provider, Some(&current), env, repository, now)
                        .await?;
                assert_order_names(names, &known)?;
            }
            Ok(Some(with_order(current, names)))
        })
        .await?;
    emit_provider_accounts_changed(provider);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn env_none(_: &str) -> Option<String> {
        None
    }

    fn names(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[tokio::test]
    async fn stores_validates_and_clears_the_account_order() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = AuthStorage::create(&tmp.path().join("auth.json").to_string_lossy());
        let repository = CredentialSlotRepository::new(
            &tmp.path()
                .join("credential-pool-state.json")
                .to_string_lossy(),
        );
        storage
            .set(
                "openai",
                Some(json!({
                    "type": "api_key",
                    "key": "key-default",
                    "accounts": [
                        { "name": "default", "key": "key-default", "source": "login" },
                        { "name": "work", "key": "key-work", "source": "login" },
                    ],
                })),
            )
            .expect("seed");

        let listing = list_credential_accounts(&storage, "openai", &env_none, &repository, 0)
            .await
            .expect("list");
        assert!(!listing.ordered);

        let order = names(&["work", "default"]);
        order_credential_accounts(&storage, "openai", Some(&order), &env_none, &repository, 0)
            .await
            .expect("order");
        assert_eq!(
            storage.get("openai").and_then(|c| c.get("order").cloned()),
            Some(json!(["work", "default"]))
        );
        let listing = list_credential_accounts(&storage, "openai", &env_none, &repository, 0)
            .await
            .expect("list");
        assert!(listing.ordered);

        for (bad, error) in [
            (names(&[]), "Account order must name at least one account"),
            (
                names(&["work", "work"]),
                "Duplicate provider account in order: work",
            ),
            (names(&["ghost"]), "Provider account not found: ghost"),
        ] {
            let result = order_credential_accounts(
                &storage,
                "openai",
                Some(&bad),
                &env_none,
                &repository,
                0,
            )
            .await;
            assert_eq!(result.err().as_deref(), Some(error));
        }
        assert_eq!(
            storage.get("openai").and_then(|c| c.get("order").cloned()),
            Some(json!(["work", "default"]))
        );

        order_credential_accounts(&storage, "openai", None, &env_none, &repository, 0)
            .await
            .expect("clear");
        assert_eq!(
            storage.get("openai").and_then(|c| c.get("order").cloned()),
            None
        );

        let missing =
            order_credential_accounts(&storage, "absent", None, &env_none, &repository, 0).await;
        assert_eq!(
            missing.err().as_deref(),
            Some("No stored credential for provider: absent")
        );
    }
}
