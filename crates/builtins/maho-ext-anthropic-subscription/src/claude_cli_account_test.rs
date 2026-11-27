//! Re-test one stored Claude CLI account, without treating the result as a block verdict.
use crate::{
    accounts::{AccountSlot, ClaudeCliAccountProfile},
    auth_lane::PROVIDER_ID,
    claude_cli_connection_test::{
        ClaudeCliAccountCheck, ClaudeCliConnectionTestInput, ConnectionTestResult,
        run_connection_test_with_profile,
    },
    claude_cli_token_source::profile_changed,
};
use maho_ai::{
    auth::types::{Credential, CredentialStore, OAuthCredential},
    utils::abort::AbortSignal,
};
use serde::Deserialize;
use serde_json::Value;
use std::{collections::BTreeMap, future::Future, path::Path};

/// Refusals before a test turn. The display value is the protocol error code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderAccountTestError {
    UnsupportedProvider,
    UnknownAccount,
    UnsupportedAccount,
    NeedsSignIn,
}

impl std::fmt::Display for ProviderAccountTestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::UnsupportedProvider => "unsupported_provider",
            Self::UnknownAccount => "unknown_account",
            Self::UnsupportedAccount => "unsupported_account",
            Self::NeedsSignIn => "needs_sign_in",
        })
    }
}

impl std::error::Error for ProviderAccountTestError {}

pub struct ClaudeCliAccountTestInput<'a> {
    pub provider: &'a str,
    pub store: &'a dyn CredentialStore,
    pub name: &'a str,
    pub model: &'a str,
    pub executable: &'a Path,
    pub cwd: &'a Path,
    pub environment: &'a BTreeMap<String, String>,
    pub agent_dir: &'a Path,
    pub signal: Option<&'a AbortSignal>,
    pub now: &'a (dyn Fn() -> u64 + Sync),
}

/// Probe the named account even while blocked. Only that sign-in's check and, if reported,
/// changed profile are merged into the latest slot. Does not change account routing or blocks.
pub async fn test_claude_cli_account(
    input: ClaudeCliAccountTestInput<'_>,
) -> anyhow::Result<ClaudeCliAccountCheck> {
    let account = account_for_test(input.store, input.provider, input.name).await?;
    crate::account_management::require_provider_consent(input.agent_dir)?;
    test_prepared_account(input, &account, run_connection_test_with_profile).await
}

async fn account_for_test(
    store: &dyn CredentialStore,
    provider: &str,
    name: &str,
) -> anyhow::Result<AccountSlot> {
    if provider != PROVIDER_ID {
        return Err(ProviderAccountTestError::UnsupportedProvider.into());
    }
    let current = store.read(PROVIDER_ID, None).await?;
    let slot = current
        .as_ref()
        .and_then(Credential::as_oauth)
        .and_then(|credential| credential.get_extra("accounts"))
        .and_then(Value::as_array)
        .and_then(|accounts| {
            accounts
                .iter()
                .find(|slot| slot["name"].as_str() == Some(name))
        })
        .ok_or(ProviderAccountTestError::UnknownAccount)?;
    if slot["store"] != "claude-cli" {
        return Err(ProviderAccountTestError::UnsupportedAccount.into());
    }
    if !slot["signInRequired"].is_null() {
        return Err(ProviderAccountTestError::NeedsSignIn.into());
    }
    Ok(serde_json::from_value(slot.clone())?)
}

async fn test_prepared_account<'a, Fut>(
    input: ClaudeCliAccountTestInput<'a>,
    account: &'a AccountSlot,
    run: impl FnOnce(ClaudeCliConnectionTestInput<'a>) -> Fut,
) -> anyhow::Result<ClaudeCliAccountCheck>
where
    Fut: Future<Output = ConnectionTestResult>,
{
    let result = run(ClaudeCliConnectionTestInput {
        account,
        model: input.model,
        executable: input.executable,
        cwd: input.cwd,
        environment: input.environment,
        agent_dir: input.agent_dir,
        signal: input.signal,
        timeout_ms: None,
        now: input.now,
    })
    .await;
    let name = account.name.clone();
    let dir_id = account.dir_id.clone();
    let check = serde_json::to_value(&result.check)?;
    let at = result.check.at;
    let profile = result.profile;
    input
        .store
        .modify(
            PROVIDER_ID,
            Box::new(move |current| {
                Box::pin(async move {
                    let Some(Credential::OAuth(mut credential)) = current else {
                        return Ok(current);
                    };
                    if let Some(slot) = same_sign_in(&mut credential, &name, dir_id.as_deref()) {
                        slot.insert("check".into(), check);
                        if let Some(mut profile) = profile {
                            let existing = slot
                                .get("profile")
                                .and_then(|value| ClaudeCliAccountProfile::deserialize(value).ok());
                            if profile_changed(existing.as_ref(), &profile) {
                                profile.checked_at = Some(at.into());
                                slot.insert("profile".into(), serde_json::to_value(profile)?);
                            }
                        }
                    }
                    Ok(Some(Credential::OAuth(credential)))
                })
            }),
            None,
        )
        .await?;
    Ok(result.check)
}

fn same_sign_in<'a>(
    credential: &'a mut OAuthCredential,
    name: &str,
    dir_id: Option<&str>,
) -> Option<&'a mut serde_json::Map<String, Value>> {
    credential
        .extra
        .get_mut("accounts")?
        .as_array_mut()?
        .iter_mut()
        .find(|slot| {
            slot["name"].as_str() == Some(name)
                && slot["store"] == "claude-cli"
                && slot["dirId"].as_str() == dir_id
        })?
        .as_object_mut()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{accounts::empty_credential, claude_cli_connection_test::ClaudeCliCheckOutcome};
    use maho_ai::auth::credential_store::InMemoryCredentialStore;
    use serde_json::json;

    const WORK_DIR: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const OTHER_DIR: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn cli_slot(name: &str, dir_id: &str) -> Value {
        json!({"name": name, "source": "login", "store": "claude-cli", "dirId": dir_id})
    }

    async fn seed(work: Value, other: Value) -> InMemoryCredentialStore {
        let store = InMemoryCredentialStore::new();
        let credential = empty_credential()
            .with_extra("accounts", json!([work, other]))
            .with_extra("pinned", json!("other"));
        store
            .modify(
                PROVIDER_ID,
                Box::new(move |_| Box::pin(async { Ok(Some(Credential::OAuth(credential))) })),
                None,
            )
            .await
            .expect("seed account store");
        store
    }

    async fn stored(store: &dyn CredentialStore) -> OAuthCredential {
        match store.read(PROVIDER_ID, None).await.expect("read fixture") {
            Some(Credential::OAuth(credential)) => credential,
            _ => panic!("expected OAuth account fixture"),
        }
    }

    fn input(store: &dyn CredentialStore) -> ClaudeCliAccountTestInput<'_> {
        static ENV: BTreeMap<String, String> = BTreeMap::new();
        ClaudeCliAccountTestInput {
            provider: PROVIDER_ID,
            store,
            name: "work",
            model: "claude-sonnet-5-5",
            executable: Path::new("must-not-spawn"),
            cwd: Path::new("must-not-open"),
            environment: &ENV,
            agent_dir: Path::new("must-not-open"),
            signal: None,
            now: &|| 123,
        }
    }

    fn scripted_result(
        probe: ClaudeCliConnectionTestInput<'_>,
        auth_error: bool,
    ) -> ConnectionTestResult {
        // This is the only account the SDK receives, even while another account is pinned.
        assert_eq!(probe.account.name, "work");
        assert_eq!(probe.account.dir_id.as_deref(), Some(WORK_DIR));
        assert_eq!(probe.model, "claude-sonnet-5-5");
        ConnectionTestResult {
            check: ClaudeCliAccountCheck {
                model: probe.model.into(),
                outcome: if auth_error {
                    ClaudeCliCheckOutcome::Failed
                } else {
                    ClaudeCliCheckOutcome::Ok
                },
                reason: auth_error.then(|| "auth_error".into()),
                message: None,
                status: auth_error.then(|| 401.into()),
                at: (probe.now)(),
            },
            profile: Some(ClaudeCliAccountProfile {
                subscription_type: Some("max".into()),
                ..Default::default()
            }),
        }
    }

    #[tokio::test]
    async fn blocked_named_account_is_checked_without_touching_its_block_or_the_pinned_account() {
        let mut work = cli_slot("work", WORK_DIR);
        work["blockedUntil"] = json!(4_102_444_000_000_i64);
        work["blockedAt"] = json!(1);
        work["blockReason"] = json!("auth_error");
        let mut other = cli_slot("other", OTHER_DIR);
        other["blockReason"] = json!("auth_error");
        other["blockedUntil"] = json!(1);
        let store = seed(work.clone(), other.clone()).await;
        let account = account_for_test(&store, PROVIDER_ID, "work").await.unwrap();

        let check = test_prepared_account(input(&store), &account, |probe| async move {
            scripted_result(probe, false)
        })
        .await
        .unwrap();

        let credential = stored(&store).await;
        let slots = credential.get_extra("accounts").unwrap();
        work["check"] = serde_json::to_value(&check).unwrap();
        work["profile"] = json!({"subscriptionType": "max", "checkedAt": 123});
        assert_eq!(slots, &json!([work, other]));
        assert_eq!(credential.get_extra_str("pinned"), Some("other"));
    }

    #[tokio::test]
    async fn failed_check_does_not_block_the_slot_or_rotate() {
        let other = cli_slot("other", OTHER_DIR);
        let store = seed(cli_slot("work", WORK_DIR), other.clone()).await;
        let account = account_for_test(&store, PROVIDER_ID, "work").await.unwrap();
        let check = test_prepared_account(input(&store), &account, |probe| async move {
            scripted_result(probe, true)
        })
        .await
        .unwrap();

        let credential = stored(&store).await;
        let slots = credential.get_extra("accounts").unwrap();
        assert_eq!(check.reason.as_deref(), Some("auth_error"));
        assert_eq!(slots[0]["check"], serde_json::to_value(check).unwrap());
        for field in ["blockReason", "blockedAt", "blockedUntil", "modelBlocks"] {
            assert!(slots[0].get(field).is_none(), "unexpected {field}");
        }
        assert_eq!(slots[1], other);
        assert_eq!(credential.get_extra_str("pinned"), Some("other"));
    }

    #[tokio::test]
    async fn concurrent_block_is_retained_and_replacement_sign_in_gets_no_stale_metadata() {
        for replacement in [false, true] {
            let store = seed(cli_slot("work", WORK_DIR), cli_slot("other", OTHER_DIR)).await;
            let account = account_for_test(&store, PROVIDER_ID, "work").await.unwrap();
            let store_ref = &store;
            test_prepared_account(input(&store), &account, move |probe| async move {
                store_ref
                    .modify(
                        PROVIDER_ID,
                        Box::new(move |current| {
                            Box::pin(async move {
                                let Some(Credential::OAuth(mut credential)) = current else {
                                    panic!("expected OAuth account fixture");
                                };
                                let slot = &mut credential.extra.get_mut("accounts").unwrap()[0];
                                if replacement {
                                    *slot = cli_slot("work", OTHER_DIR);
                                } else {
                                    slot["blockReason"] = json!("billing");
                                    slot["blockedAt"] = json!(2);
                                    slot["blockedUntil"] = json!(4_102_444_000_000_i64);
                                }
                                Ok(Some(Credential::OAuth(credential)))
                            })
                        }),
                        None,
                    )
                    .await
                    .unwrap();
                scripted_result(probe, false)
            })
            .await
            .unwrap();

            let credential = stored(&store).await;
            let work = &credential.get_extra("accounts").unwrap()[0];
            if replacement {
                assert_eq!(work, &cli_slot("work", OTHER_DIR));
            } else {
                assert_eq!(work["blockReason"], "billing");
                assert_eq!(work["blockedAt"], 2);
                assert_eq!(work["blockedUntil"], 4_102_444_000_000_i64);
                assert_eq!(work["check"]["outcome"], "ok");
            }
        }
    }

    #[tokio::test]
    async fn invalid_targets_are_refused_before_the_probe_or_store_mutation() {
        let mut work = cli_slot("work", WORK_DIR);
        work["signInRequired"] = json!("login_expired");
        let store = seed(work, json!({"name": "other", "source": "login"})).await;
        let before = stored(&store).await;
        for (provider, name, error) in [
            (
                "openai",
                "work",
                ProviderAccountTestError::UnsupportedProvider,
            ),
            (
                PROVIDER_ID,
                "missing",
                ProviderAccountTestError::UnknownAccount,
            ),
            (
                PROVIDER_ID,
                "other",
                ProviderAccountTestError::UnsupportedAccount,
            ),
            (PROVIDER_ID, "work", ProviderAccountTestError::NeedsSignIn),
        ] {
            let result = test_claude_cli_account(ClaudeCliAccountTestInput {
                provider,
                name,
                ..input(&store)
            })
            .await;
            assert_eq!(
                result
                    .unwrap_err()
                    .downcast_ref::<ProviderAccountTestError>(),
                Some(&error)
            );
        }
        assert_eq!(stored(&store).await, before);
    }

    #[tokio::test]
    async fn retest_without_recorded_consent_does_not_write_a_check() {
        let store = seed(cli_slot("work", WORK_DIR), cli_slot("other", OTHER_DIR)).await;
        let before = stored(&store).await;
        // A relative agent dir cannot provide global consent and is rejected without a disk read.
        let error = test_claude_cli_account(input(&store)).await.unwrap_err();
        assert_eq!(
            error.to_string(),
            maho_core::provider_consent::consent_required_message(PROVIDER_ID)
        );
        assert_eq!(stored(&store).await, before);
    }
}
