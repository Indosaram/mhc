//! Writes and rollback for a CLI-owned login, scoped to the exact account dir.

use crate::{
    accounts::{AccountSlot, AccountSource, AccountStore, ClaudeCliAccountProfile},
    claude_cli_auth_status::ClaudeCliProfile,
};
use maho_ai::auth::types::OAuthCredential;
use serde_json::Value;

pub(crate) fn signed_in_slot(
    previous: Option<&AccountSlot>,
    name: String,
    dir_id: String,
    profile: ClaudeCliProfile,
) -> AccountSlot {
    let at = serde_json::Number::from(maho_ai::utils::diagnostics::now_ms());
    AccountSlot {
        name,
        display_name: previous.and_then(|slot| slot.display_name.clone()),
        source: AccountSource::Login,
        store: Some(AccountStore::ClaudeCli),
        dir_id: Some(dir_id),
        signed_in_at: Some(at.clone()),
        profile: Some(ClaudeCliAccountProfile {
            email: profile.email,
            org_name: profile.org_name,
            subscription_type: profile.subscription_type,
            checked_at: Some(at),
        }),
        model_blocks: previous.and_then(|slot| slot.model_blocks.clone()),
        ..Default::default()
    }
}

pub(crate) fn with_slot(
    credential: OAuthCredential,
    slot: &AccountSlot,
) -> anyhow::Result<OAuthCredential> {
    let mut accounts = crate::accounts::stored_slots(&credential)?;
    match accounts
        .iter_mut()
        .find(|candidate| candidate.name == slot.name)
    {
        Some(candidate) => *candidate = slot.clone(),
        None => accounts.push(slot.clone()),
    }
    Ok(credential.with_extra("accounts", serde_json::to_value(accounts)?))
}

pub(crate) fn with_check(
    credential: OAuthCredential,
    slot: &AccountSlot,
    check: Value,
    profile: Option<ClaudeCliAccountProfile>,
) -> anyhow::Result<OAuthCredential> {
    let mut accounts = crate::accounts::stored_slots(&credential)?;
    if let Some(candidate) = accounts
        .iter_mut()
        .find(|candidate| same_login(candidate, slot))
    {
        candidate.check = Some(check);
        if let Some(mut profile) = profile
            && crate::claude_cli_token_source::profile_changed(candidate.profile.as_ref(), &profile)
        {
            profile.checked_at = Some(maho_ai::utils::diagnostics::now_ms().into());
            candidate.profile = Some(profile);
        }
    }
    Ok(credential.with_extra("accounts", serde_json::to_value(accounts)?))
}

fn same_login(current: &AccountSlot, written: &AccountSlot) -> bool {
    current.name == written.name && current.is_claude_cli() && current.dir_id == written.dir_id
}

pub(crate) fn without_this_login(
    credential: OAuthCredential,
    written: &AccountSlot,
    previous: Option<&AccountSlot>,
) -> anyhow::Result<OAuthCredential> {
    let mut accounts = crate::accounts::stored_slots(&credential)?;
    let Some(index) = accounts
        .iter()
        .position(|candidate| same_login(candidate, written))
    else {
        return Ok(credential);
    };
    if let Some(previous) = previous {
        const LOGIN_FIELDS: &[&str] =
            &["dirId", "signedInAt", "profile", "check", "signInRequired"];
        let mut current = serde_json::to_value(&accounts[index])?;
        let ours = serde_json::to_value(written)?;
        let before = serde_json::to_value(previous)?;
        let (Value::Object(restored), Value::Object(ours), Value::Object(before)) =
            (&mut current, ours, before)
        else {
            anyhow::bail!("Claude account slot was not serialized as an object");
        };
        for key in ours
            .keys()
            .chain(before.keys())
            .map(String::as_str)
            .chain(LOGIN_FIELDS.iter().copied())
        {
            if LOGIN_FIELDS.contains(&key) || restored.get(key) == ours.get(key) {
                if let Some(value) = before.get(key) {
                    restored.insert(key.to_owned(), value.clone());
                } else {
                    restored.remove(key);
                }
            }
        }
        accounts[index] = serde_json::from_value(current)?;
    } else {
        accounts.remove(index);
    }
    Ok(credential.with_extra("accounts", serde_json::to_value(accounts)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::{empty_credential, stored_slots};

    #[test]
    fn rollback_restores_login_bound_fields_but_preserves_concurrent_account_changes() {
        let old = AccountSlot {
            name: "work".into(),
            store: Some(AccountStore::ClaudeCli),
            dir_id: Some("old".into()),
            profile: Some(ClaudeCliAccountProfile {
                subscription_type: Some("old plan".into()),
                ..Default::default()
            }),
            check: Some(serde_json::json!({"outcome":"ok"})),
            ..Default::default()
        };
        let written = signed_in_slot(
            Some(&old),
            "work".into(),
            "fresh".into(),
            ClaudeCliProfile::default(),
        );
        let mut concurrent = written.clone();
        concurrent.display_name = Some("Renamed while signing in".into());
        concurrent.blocked_until = Some(200.0);
        concurrent.block_reason = Some("rate_limit".into());
        concurrent.check = Some(serde_json::json!({"outcome":"failed"}));
        let pool = with_slot(empty_credential(), &concurrent).expect("concurrent write");
        let restored = without_this_login(pool, &written, Some(&old)).expect("rollback");
        let slots = stored_slots(&restored).expect("slots");
        let slot = &slots[0];
        assert_eq!(slot.dir_id, old.dir_id);
        assert_eq!(slot.profile, old.profile);
        assert_eq!(slot.check, old.check);
        assert_eq!(slot.display_name, concurrent.display_name);
        assert_eq!(slot.blocked_until, concurrent.blocked_until);
        assert_eq!(slot.block_reason, concurrent.block_reason);
    }

    #[test]
    fn stale_rollback_and_probe_cannot_change_a_replacement_login() {
        let losing = signed_in_slot(
            None,
            "work".into(),
            "losing".into(),
            ClaudeCliProfile::default(),
        );
        let winner = signed_in_slot(
            None,
            "work".into(),
            "winner".into(),
            ClaudeCliProfile::default(),
        );
        let pool = with_slot(empty_credential(), &winner).expect("winner");
        let stale_check = serde_json::json!({"outcome":"failed"});
        assert_eq!(
            with_check(pool.clone(), &losing, stale_check, None).expect("stale check"),
            pool,
        );
        assert_eq!(
            without_this_login(pool.clone(), &losing, None).expect("stale rollback"),
            pool,
        );
    }
}
