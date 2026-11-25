use maho_ai::{
    auth::types::{Credential, CredentialStore, OAuthCredential},
    utils::abort::AbortSignal,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::future::Future;

pub const SENTINEL_TOKEN: &str = "claude-sdk-oauth-managed";
pub const SENTINEL_EXPIRES: f64 = 4102444800000.0;
/// A slot is either a token slot (an env `CLAUDE_CODE_OAUTH_TOKEN[_N]` account, or one the pre-#4436 senpi-owned
/// login stored) or a claude CLI account (`store: "claude-cli"`, senpi#4436) that holds no token: the CLI keeps the
/// login in its own credential store, keyed on the account dir `account_dir_for(agent_dir, dir_id)`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AccountSlot {
    /// Empty only on a stored claude CLI account that names no account; such a slot is never listed (omo#12228).
    #[serde(default)]
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Empty on a claude CLI account, which never holds token material.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub refresh: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub access: String,
    #[serde(default, skip_serializing_if = "is_unset_expiry")]
    pub expires: f64,
    #[serde(default)]
    pub source: AccountSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store: Option<AccountStore>,
    /// 32 hex chars, fixed at creation (claude CLI accounts only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signed_in_at: Option<serde_json::Number>,
    /// The local-only profile `claude auth status` reported; never logged or sent to telemetry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<ClaudeCliAccountProfile>,
    /// The sign-in connection test's last result, kept as stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<Value>,
    /// Never selected for a turn. `login_expired`: a due auth re-probe found the CLI signed out (senpi#5350); the slot
    /// keeps its `auth_error` block so the probe keeps running, and a `signed_in` probe or `/login` clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sign_in_required: Option<SignInRequiredReason>,
    /// Set with an `auth_error` block this engine reached about the sign-in in `dir_id` (senpi#5623).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_verdict: Option<ClaudeCliAuthVerdict>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked_until: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block_reason: Option<String>,
    /// When the current block was stored, so a turn that started earlier never clears it (senpi#3462).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_at: Option<f64>,
    /// Rate limits that bind one model family on this account, not the account (senpi#2555).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_blocks: Option<crate::model_blocks::ModelBlocks>,
    /// Digest of the grant an auth-block recovery must not redeem again (senpi#2926).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_recovery_grant: Option<String>,
    /// Digest of the `anthropic` OAuth grant this account was copied from (senpi#3259).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imported_from: Option<String>,
    /// Fields this port does not model (written by a newer engine), kept so a rewrite never drops them.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}
fn is_unset_expiry(expires: &f64) -> bool {
    *expires == 0.0
}
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AccountSource {
    #[default]
    Login,
    Import,
    Env,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum AccountStore {
    #[serde(rename = "claude-cli")]
    ClaudeCli,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SignInRequiredReason {
    LegacySignInRetired,
    LoginExpired,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeCliAccountProfile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub org_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscription_type: Option<String>,
    /// Kept as stored (omo writes integer milliseconds), so a rewrite never turns it into a float.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<serde_json::Number>,
}
/// The sign-in an `auth_error` block on a Claude CLI slot is about: its account dir and the block's `blockedAt`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeCliAuthVerdict {
    pub dir_id: String,
    pub at: f64,
}
impl AccountSlot {
    /// `slot.store === "claude-cli"`: an account signed in through the claude CLI.
    pub fn is_claude_cli(&self) -> bool {
        self.store == Some(AccountStore::ClaudeCli)
    }
}
/// Whether a Claude CLI slot carries an `auth_error` block that no Claude CLI verdict backs (senpi#5623). An engine from
/// before #4436 lists the slot, finds no token in it, and writes `blockReason`/`blockedUntil`/`blockedAt` onto it
/// without contacting Claude; such a block says nothing about the login in the slot's dir.
pub fn is_foreign_auth_block(slot: &AccountSlot) -> bool {
    if !slot.is_claude_cli() || slot.block_reason.as_deref() != Some("auth_error") {
        return false;
    }
    // Only this engine marks a login expired (senpi#5350), so that block is never another engine's.
    if slot.sign_in_required == Some(SignInRequiredReason::LoginExpired) {
        return false;
    }
    slot.auth_verdict.as_ref().is_none_or(|verdict| {
        Some(&verdict.dir_id) != slot.dir_id.as_ref() || Some(verdict.at) != slot.blocked_at
    })
}
fn without_foreign_auth_block(mut slot: AccountSlot) -> AccountSlot {
    if is_foreign_auth_block(&slot) {
        slot.blocked_until = None;
        slot.block_reason = None;
        slot.blocked_at = None;
        slot.auth_verdict = None;
    }
    slot
}
/// Whether a stored slot is listed. A claude CLI account is listed when it names an account (senpi#4436); a token slot
/// is listed unless it is a sentinel projection.
fn is_listed_stored_slot(slot: &AccountSlot) -> bool {
    if slot.is_claude_cli() {
        return !slot.name.trim().is_empty();
    }
    !is_sentinel_slot(slot)
}
/// A stored claude CLI account the listing rule drops, so a lane can refuse the turn instead of silently falling back to
/// ambient host credentials when every such account is unusable (oh-my-openagent#12228).
pub fn unusable_stored_slot_count(credential: &OAuthCredential) -> usize {
    credential
        .extra
        .get("accounts")
        .and_then(Value::as_array)
        .map_or(0, |stored| {
            stored
                .iter()
                .filter(|slot| {
                    slot.get("store").and_then(Value::as_str) == Some("claude-cli")
                        && slot
                            .get("name")
                            .and_then(Value::as_str)
                            .is_none_or(|name| name.trim().is_empty())
                })
                .count()
        })
}
pub fn empty_credential() -> OAuthCredential {
    OAuthCredential::new(SENTINEL_TOKEN, SENTINEL_TOKEN, SENTINEL_EXPIRES)
        .with_extra("accounts", serde_json::json!([]))
}
fn stored_slots(credential: &OAuthCredential) -> anyhow::Result<Vec<AccountSlot>> {
    match credential.extra.get("accounts") {
        Some(value) => Ok(serde_json::from_value(value.clone())?),
        None => Ok(Vec::new()),
    }
}
pub fn is_sentinel_slot(slot: &AccountSlot) -> bool {
    slot.access == SENTINEL_TOKEN && slot.refresh == SENTINEL_TOKEN
}
pub type EnvironmentReader<'a> = dyn Fn(&str) -> Option<String> + 'a;
pub fn list_accounts(
    credential: &OAuthCredential,
    env: Option<&EnvironmentReader<'_>>,
) -> anyhow::Result<Vec<AccountSlot>> {
    let mut slots: Vec<_> = stored_slots(credential)?
        .into_iter()
        .filter(is_listed_stored_slot)
        .map(without_foreign_auth_block)
        .collect();
    if let Some(env) = env {
        for mut slot in env_slots(env) {
            if let Some(state) = credential
                .extra
                .get("slotState")
                .and_then(|state| state.get(&slot.name))
            {
                slot.blocked_until = state["blockedUntil"].as_f64();
                slot.block_reason = state["blockReason"].as_str().map(str::to_owned);
                slot.blocked_at = state["blockedAt"].as_f64();
                slot.model_blocks = state
                    .get("modelBlocks")
                    .and_then(|blocks| serde_json::from_value(blocks.clone()).ok());
            }
            slots.push(slot);
        }
    }
    Ok(slots)
}
pub fn assert_valid_account_name(name: &str) -> anyhow::Result<()> {
    let mut bytes = name.bytes();
    if !(1..=64).contains(&name.len())
        || !bytes.next().is_some_and(|b| b.is_ascii_alphanumeric())
        || !bytes.all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        anyhow::bail!(
            "Invalid account name '{name}': use letters, digits, '-' or '_', starting with a letter or digit"
        );
    }
    Ok(())
}
pub fn add_account(
    credential: &OAuthCredential,
    slot: AccountSlot,
) -> anyhow::Result<OAuthCredential> {
    assert_valid_account_name(&slot.name)?;
    let mut accounts = stored_slots(credential)?;
    if accounts.iter().any(|a| a.name == slot.name) {
        anyhow::bail!("Account '{}' already exists", slot.name);
    }
    accounts.push(slot);
    Ok(credential
        .clone()
        .with_extra("accounts", serde_json::to_value(accounts)?))
}
pub fn upsert_account(
    credential: &OAuthCredential,
    slot: AccountSlot,
) -> anyhow::Result<OAuthCredential> {
    assert_valid_account_name(&slot.name)?;
    let mut accounts = stored_slots(credential)?;
    match accounts.iter_mut().find(|a| a.name == slot.name) {
        Some(existing) => {
            // A token login onto a claude CLI account's name replaces that account with the token slot.
            existing.store = None;
            existing.dir_id = None;
            existing.signed_in_at = None;
            existing.profile = None;
            existing.check = None;
            existing.sign_in_required = None;
            existing.auth_verdict = None;
            existing.access = slot.access;
            existing.refresh = slot.refresh;
            existing.expires = slot.expires;
            existing.source = slot.source;
            existing.blocked_until = None;
            existing.block_reason = None;
            existing.blocked_at = None;
            existing.model_blocks = None;
            existing.auth_recovery_grant = None;
        }
        None => accounts.push(slot),
    }
    Ok(credential
        .clone()
        .with_extra("accounts", serde_json::to_value(accounts)?))
}
pub fn remove_account(credential: &OAuthCredential, name: &str) -> anyhow::Result<OAuthCredential> {
    let accounts: Vec<_> = stored_slots(credential)?
        .into_iter()
        .filter(|a| a.name != name)
        .collect();
    let mut next = credential
        .clone()
        .with_extra("accounts", serde_json::to_value(accounts)?);
    if credential.get_extra_str("pinned") == Some(name) {
        next.extra.remove("pinned");
    }
    Ok(without_ordered_account(next, name))
}
/// `withoutOrderedAccount`: drops a removed account from the user's stored order (senpi#3591), and the
/// order itself once it is empty.
pub fn without_ordered_account(mut credential: OAuthCredential, name: &str) -> OAuthCredential {
    let Some(order) = credential
        .get_extra("order")
        .and_then(|order| serde_json::from_value::<Vec<String>>(order.clone()).ok())
        .filter(|order| order.iter().any(|entry| entry == name))
    else {
        return credential;
    };
    let order: Vec<String> = order.into_iter().filter(|entry| entry != name).collect();
    if order.is_empty() {
        credential.extra.remove("order");
    } else {
        credential
            .extra
            .insert("order".into(), serde_json::json!(order));
    }
    credential
}
pub fn pin_account(credential: &OAuthCredential, name: &str) -> OAuthCredential {
    credential
        .clone()
        .with_extra("pinned", Value::String(name.into()))
}
pub fn assert_sentinel_invariant(credential: &OAuthCredential) -> anyhow::Result<()> {
    if credential.access != SENTINEL_TOKEN
        || credential.refresh != SENTINEL_TOKEN
        || (credential.expires - SENTINEL_EXPIRES).abs() > f64::EPSILON
    {
        anyhow::bail!("top-level OAuth fields must remain sentinel values");
    }
    Ok(())
}
pub fn env_slots(env: &dyn Fn(&str) -> Option<String>) -> Vec<AccountSlot> {
    (1..=16)
        .filter_map(|index| {
            let key = if index == 1 {
                "CLAUDE_CODE_OAUTH_TOKEN".into()
            } else {
                format!("CLAUDE_CODE_OAUTH_TOKEN_{index}")
            };
            env(&key).filter(|v| !v.is_empty()).map(|_| AccountSlot {
                name: if index == 1 {
                    "env".into()
                } else {
                    format!("env-{index}")
                },
                display_name: None,
                refresh: String::new(),
                access: String::new(),
                expires: 0.0,
                source: AccountSource::Env,
                blocked_until: None,
                block_reason: None,
                ..Default::default()
            })
        })
        .collect()
}
pub fn env_slot_token(env: &dyn Fn(&str) -> Option<String>, name: &str) -> Option<String> {
    if name == "env" {
        return env("CLAUDE_CODE_OAUTH_TOKEN");
    }
    let suffix = name.strip_prefix("env-")?;
    if suffix.is_empty() || !suffix.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    env(&format!("CLAUDE_CODE_OAUTH_TOKEN_{suffix}"))
}
pub async fn refresh_slot<F, Fut>(
    store: &dyn CredentialStore,
    provider: &str,
    name: &str,
    refresh: F,
    signal: AbortSignal,
    is_expiring: impl Fn(f64) -> bool + Send + 'static,
) -> anyhow::Result<Option<Credential>>
where
    F: FnOnce(String, AbortSignal) -> Fut + Send + 'static,
    Fut: Future<Output = anyhow::Result<OAuthCredential>> + Send + 'static,
{
    let name = name.to_owned();
    store
        .modify(
            provider,
            Box::new(move |current| {
                Box::pin(async move {
                    let Some(Credential::OAuth(credential)) = current else {
                        return Ok(None);
                    };
                    let mut accounts = stored_slots(&credential)?;
                    let Some(slot) = accounts.iter().find(|a| a.name == name) else {
                        return Ok(Some(Credential::OAuth(credential)));
                    };
                    if !is_expiring(slot.expires) {
                        return Ok(Some(Credential::OAuth(credential)));
                    }
                    let refreshed = refresh(slot.refresh.clone(), signal).await?;
                    for slot in &mut accounts {
                        if slot.name == name {
                            slot.access.clone_from(&refreshed.access);
                            slot.refresh.clone_from(&refreshed.refresh);
                            slot.expires = refreshed.expires;
                        }
                    }
                    Ok(Some(Credential::OAuth(
                        credential.with_extra("accounts", serde_json::to_value(accounts)?),
                    )))
                })
            }),
            None,
        )
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    fn slot(name: &str) -> AccountSlot {
        AccountSlot {
            name: name.into(),
            display_name: None,
            refresh: "rA".into(),
            access: "aA".into(),
            expires: 60000.0,
            source: AccountSource::Login,
            blocked_until: None,
            block_reason: None,
            ..Default::default()
        }
    }
    #[test]
    fn block_fields_round_trip_and_a_relogin_clears_them() {
        let mut blocked = slot("work");
        blocked.blocked_until = Some(9.0);
        blocked.block_reason = Some("rate_limit".into());
        blocked.blocked_at = Some(1.0);
        blocked.model_blocks = Some(
            [(
                "opus".to_owned(),
                crate::model_blocks::ModelBlock { blocked_until: 7.0 },
            )]
            .into(),
        );
        blocked.auth_recovery_grant = Some("grant".into());
        blocked.imported_from = Some("source".into());
        let credential = add_account(&empty_credential(), blocked.clone()).unwrap();
        assert_eq!(
            credential.extra["accounts"][0]["modelBlocks"]["opus"]["blockedUntil"],
            7.0
        );
        assert_eq!(list_accounts(&credential, None).unwrap(), [blocked.clone()]);
        let env=credential.clone().with_extra("slotState",serde_json::json!({"env":{"blockedUntil":5.0,"blockReason":"rate_limit","blockedAt":2.0,"modelBlocks":{"fable":{"blockedUntil":6.0}}}}));
        let env_slot = list_accounts(
            &env,
            Some(&|name: &str| (name == "CLAUDE_CODE_OAUTH_TOKEN").then(|| "token".to_owned())),
        )
        .unwrap()
        .pop()
        .unwrap();
        assert_eq!(
            (
                env_slot.blocked_at,
                env_slot
                    .model_blocks
                    .map(|blocks| blocks["fable"].blocked_until)
            ),
            (Some(2.0), Some(6.0))
        );
        let relogin = list_accounts(&upsert_account(&credential, slot("work")).unwrap(), None)
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(
            (
                relogin.blocked_at,
                relogin.model_blocks,
                relogin.auth_recovery_grant,
                relogin.imported_from
            ),
            (None, None, None, Some("source".into()))
        );
    }
    /// The shape omo 5.1.81 (senpi v2026.11.19-5) writes for claude CLI accounts, with synthetic values.
    fn claude_cli_credential() -> OAuthCredential {
        empty_credential()
            .with_extra("loginId", serde_json::json!("login-1"))
            .with_extra(
                "accounts",
                serde_json::json!([
                    {"name":"default","source":"login","store":"claude-cli","dirId":"0123456789abcdef0123456789abcdef","signedInAt":1,
                     "profile":{"email":"a@example.com","orgName":"Org","subscriptionType":"max","checkedAt":1},
                     "check":{"model":"claude-opus-5-5","outcome":"ok","at":2},"futureField":{"kept":true}},
                    {"name":"stale","source":"login","store":"claude-cli","dirId":"fedcba9876543210fedcba9876543210",
                     "blockReason":"auth_error","blockedUntil":99,"blockedAt":9},
                    {"name":" ","source":"login","store":"claude-cli","dirId":"00000000000000000000000000000000"}
                ]),
            )
    }
    #[test]
    fn claude_cli_slots_load_list_without_foreign_blocks_and_round_trip() {
        let credential = claude_cli_credential();
        let listed = list_accounts(&credential, None).unwrap();
        assert_eq!(
            listed
                .iter()
                .map(|slot| slot.name.as_str())
                .collect::<Vec<_>>(),
            ["default", "stale"]
        );
        assert!(listed.iter().all(AccountSlot::is_claude_cli));
        assert_eq!(
            listed[0]
                .profile
                .as_ref()
                .and_then(|profile| profile.org_name.as_deref()),
            Some("Org")
        );
        // An auth block no claude CLI verdict backs came from another engine and says nothing about the login.
        assert_eq!(
            (listed[1].block_reason.as_deref(), listed[1].blocked_until),
            (None, None)
        );
        assert_eq!(unusable_stored_slot_count(&credential), 1);
        let written = remove_account(&credential, "stale").unwrap();
        let stored = &written.extra["accounts"][0];
        assert_eq!(stored, &credential.extra["accounts"][0]);
        assert!(stored.get("refresh").is_none() && stored.get("access").is_none());
        assert_eq!(written.extra["accounts"][1]["name"], " ");
        assert_eq!(written.get_extra_str("loginId"), Some("login-1"));
        let mut verdict = credential.extra["accounts"][1].clone();
        verdict["authVerdict"] =
            serde_json::json!({"dirId":"fedcba9876543210fedcba9876543210","at":9});
        let own = empty_credential().with_extra("accounts", serde_json::json!([verdict]));
        assert_eq!(
            list_accounts(&own, None).unwrap()[0]
                .block_reason
                .as_deref(),
            Some("auth_error")
        );
    }
    #[test]
    fn a_token_login_onto_a_claude_cli_name_replaces_the_cli_account() {
        let replaced = upsert_account(&claude_cli_credential(), slot("default")).unwrap();
        let account = &list_accounts(&replaced, None).unwrap()[0];
        assert!(!account.is_claude_cli());
        assert_eq!(
            (account.dir_id.as_ref(), account.access.as_str()),
            (None, "aA")
        );
    }
    #[test]
    fn empty_sentinel() {
        let credential = empty_credential();
        assert_sentinel_invariant(&credential).unwrap();
        assert!(list_accounts(&credential, None).unwrap().is_empty());
    }
    #[test]
    fn removing_an_account_drops_it_from_the_stored_order() {
        let credential = add_account(
            &add_account(&empty_credential(), slot("default")).unwrap(),
            slot("work"),
        )
        .unwrap();
        let ordered = credential
            .clone()
            .with_extra("order", serde_json::json!(["work", "default"]));
        assert_eq!(
            remove_account(&ordered, "default")
                .unwrap()
                .get_extra("order"),
            Some(&serde_json::json!(["work"]))
        );
        let only = credential
            .clone()
            .with_extra("order", serde_json::json!(["work"]));
        assert_eq!(
            remove_account(&only, "work").unwrap().get_extra("order"),
            None
        );
        let unrelated = credential.with_extra("order", serde_json::json!(["work"]));
        assert_eq!(
            remove_account(&unrelated, "default")
                .unwrap()
                .get_extra("order"),
            Some(&serde_json::json!(["work"]))
        );
    }
    #[test]
    fn add_list_pin_remove() {
        let credential = add_account(
            &add_account(&empty_credential(), slot("default")).unwrap(),
            slot("work"),
        )
        .unwrap();
        assert_eq!(list_accounts(&credential, None).unwrap().len(), 2);
        let removed = remove_account(&pin_account(&credential, "work"), "default").unwrap();
        assert_eq!(list_accounts(&removed, None).unwrap(), [slot("work")]);
    }
    #[test]
    fn duplicate_rejected() {
        assert!(
            add_account(
                &add_account(&empty_credential(), slot("default")).unwrap(),
                slot("default")
            )
            .is_err()
        );
    }
    #[test]
    fn operations_preserve_sentinel() {
        let credential = remove_account(
            &pin_account(
                &add_account(&empty_credential(), slot("default")).unwrap(),
                "default",
            ),
            "default",
        )
        .unwrap();
        assert_sentinel_invariant(&credential).unwrap();
    }
    #[test]
    fn env_slots_never_store_tokens() {
        let slots = env_slots(&|key| match key {
            "CLAUDE_CODE_OAUTH_TOKEN" | "CLAUDE_CODE_OAUTH_TOKEN_2" => Some("secret".into()),
            _ => None,
        });
        assert_eq!(
            slots.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["env", "env-2"]
        );
        assert!(!serde_json::to_string(&slots).unwrap().contains("secret"));
    }
    #[test]
    fn env_state_rehydrated() {
        let credential = empty_credential().with_extra(
            "slotState",
            serde_json::json!({"env":{"blockedUntil":123,"blockReason":"rate_limit"}}),
        );
        let slots = list_accounts(
            &credential,
            Some(&|key| (key == "CLAUDE_CODE_OAUTH_TOKEN").then(|| "secret".into())),
        )
        .unwrap();
        assert_eq!(slots[0].blocked_until, Some(123.0));
        assert_eq!(slots[0].block_reason.as_deref(), Some("rate_limit"));
    }
    #[test]
    fn poisoned_sentinel_slot_never_listed() {
        let mut poisoned = slot("login-2");
        poisoned.access = SENTINEL_TOKEN.into();
        poisoned.refresh = SENTINEL_TOKEN.into();
        let credential = add_account(
            &add_account(&empty_credential(), slot("default")).unwrap(),
            poisoned,
        )
        .unwrap();
        assert_eq!(list_accounts(&credential, None).unwrap(), [slot("default")]);
    }
    #[test]
    fn relogin_lifts_auth_block_preserves_display_name() {
        let mut existing = slot("default");
        existing.display_name = Some("Primary".into());
        existing.block_reason = Some("auth_error".into());
        existing.blocked_until = Some(999.0);
        let credential = upsert_account(
            &add_account(&empty_credential(), existing).unwrap(),
            slot("default"),
        )
        .unwrap();
        let account = &list_accounts(&credential, None).unwrap()[0];
        assert_eq!(account.display_name.as_deref(), Some("Primary"));
        assert_eq!(account.block_reason, None);
        assert_eq!(account.blocked_until, None);
    }
    #[tokio::test]
    async fn refreshes_only_named_slot() {
        use maho_ai::{
            auth::credential_store::InMemoryCredentialStore, utils::abort::AbortController,
        };
        let store = InMemoryCredentialStore::new();
        let credential = add_account(
            &add_account(&empty_credential(), slot("default")).unwrap(),
            slot("other"),
        )
        .unwrap();
        store
            .modify(
                "anthropic-subscription",
                Box::new(move |_| Box::pin(async { Ok(Some(Credential::OAuth(credential))) })),
                None,
            )
            .await
            .unwrap();
        let result = refresh_slot(
            &store,
            "anthropic-subscription",
            "default",
            |refresh, _| async move {
                Ok(OAuthCredential::new(
                    "new",
                    format!("{refresh}-new"),
                    120000.0,
                ))
            },
            AbortController::new().signal(),
            |_| true,
        )
        .await
        .unwrap()
        .unwrap()
        .into_oauth()
        .unwrap();
        let accounts = list_accounts(&result, None).unwrap();
        assert_eq!(accounts[0].access, "new");
        assert_eq!(accounts[1], slot("other"));
        assert_sentinel_invariant(&result).unwrap();
    }
}
