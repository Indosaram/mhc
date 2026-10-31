use crate::accounts::AccountSlot;
use sha2::{Digest, Sha256};
pub const DEFAULT_AFFINITY_KEY: &str = "claude-sdk-oauth-default";
#[derive(Default)]
pub struct AffinityOptions<'a> {
    pub affinity_key: Option<&'a str>,
    pub session_id: Option<&'a str>,
    pub pinned_account: Option<&'a str>,
    /// Where `pinned_account` came from, so guidance for an unusable pin says where to change it.
    pub pin_source: Option<PinSource>,
    /// With `false`, only one account may serve (senpi#3465).
    pub account_fallback: Option<bool>,
    /// The account this session's SDK transcript lives under; kept while it can serve (senpi#2891).
    pub preferred_account: Option<&'a str>,
    /// The user's account order (senpi#3591): tried after the pin and the session's own account, before HRW.
    pub account_order: Option<&'a [String]>,
    /// The requested model; an account blocked only for another model stays eligible for it (senpi#2555).
    pub model: Option<&'a str>,
}
/// The origin of an account pin, highest precedence first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinSource {
    Cli,
    Settings,
    Stored,
}
/// The one account a request may use when account fallback is off.
#[derive(Clone, Debug, PartialEq)]
pub struct UnavailableAccount {
    pub name: String,
    pub configured: bool,
    /// Set when the account is a pin: a CLI or settings pin outranks `/claude-account pin`.
    pub pinned_by: Option<PinSource>,
}
#[derive(Debug, PartialEq)]
pub struct AllAccountsBlockedError {
    pub soonest_unblock_at: Option<f64>,
    pub block_reason: Option<&'static str>,
    pub unavailable_account: Option<UnavailableAccount>,
    /// The requested model when a usage limit on that model, not the accounts, is what blocks it somewhere.
    pub limited_model: Option<String>,
}
impl std::fmt::Display for AllAccountsBlockedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let (Some(model), Some(at)) = (&self.limited_model, self.soonest_unblock_at) {
            return write!(
                f,
                "All Anthropic Subscription accounts have hit the usage limit for model {model} until {}.",
                crate::model_blocks::iso_ms(at)
            );
        }
        match self.soonest_unblock_at {
            None => f.write_str("All Anthropic Subscription accounts are blocked until re-login."),
            Some(at) => {
                let date =
                    chrono::DateTime::from_timestamp_millis(at as i64).ok_or(std::fmt::Error)?;
                write!(
                    f,
                    "All Anthropic Subscription accounts are blocked until {}.",
                    date.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
                )
            }
        }
    }
}
impl std::error::Error for AllAccountsBlockedError {}
pub fn get_affinity_key<'a>(options: &AffinityOptions<'a>) -> &'a str {
    options
        .affinity_key
        .or(options.session_id)
        .unwrap_or(DEFAULT_AFFINITY_KEY)
}
fn score(key: &str, name: &str) -> u64 {
    let mut hash = Sha256::new();
    hash.update(key.as_bytes());
    hash.update([0]);
    hash.update(name.as_bytes());
    let digest = hash.finalize();
    u64::from_be_bytes([
        digest[0], digest[1], digest[2], digest[3], digest[4], digest[5], digest[6], digest[7],
    ])
}
pub fn rendezvous_order(key: &str, accounts: &[AccountSlot]) -> Vec<AccountSlot> {
    let mut ranked: Vec<_> = accounts
        .iter()
        .map(|account| (score(key, &account.name), account))
        .collect();
    ranked.sort_by_key(|entry| std::cmp::Reverse(entry.0));
    ranked
        .into_iter()
        .map(|(_, account)| account.clone())
        .collect()
}
fn is_account_blocked(account: &AccountSlot, now: f64) -> bool {
    account.blocked_until.is_some_and(|until| until > now)
}
/// Whether the account cannot serve `model` now: an account-level block or a live block on that model.
pub fn is_blocked_for(account: &AccountSlot, now: f64, model: Option<&str>) -> bool {
    is_account_blocked(account, now)
        || crate::model_blocks::active_model_block_until(account.model_blocks.as_ref(), model, now)
            .is_some()
}
/// Moves the named accounts, in the user's order, ahead of the HRW ranking; unknown names are skipped.
pub fn apply_account_order(order: Option<&[String]>, ranked: Vec<AccountSlot>) -> Vec<AccountSlot> {
    let Some(order) = order.filter(|order| !order.is_empty()) else {
        return ranked;
    };
    let mut front: Vec<AccountSlot> = Vec::new();
    for name in order {
        if !front.iter().any(|slot| &slot.name == name)
            && let Some(slot) = ranked.iter().find(|slot| &slot.name == name)
        {
            front.push(slot.clone());
        }
    }
    let rest: Vec<_> = ranked
        .into_iter()
        .filter(|slot| !front.iter().any(|f| f.name == slot.name))
        .collect();
    front.extend(rest);
    front
}
pub fn clear_expired_blocks(accounts: &[AccountSlot], now: f64) -> Vec<AccountSlot> {
    accounts
        .iter()
        .map(|account| {
            let mut account = account.clone();
            if account.block_reason.is_some()
                && account.blocked_until.is_none_or(|until| until <= now)
            {
                account.blocked_until = None;
                account.block_reason = None;
                account.blocked_at = None;
            }
            account.model_blocks =
                crate::model_blocks::prune_model_blocks(account.model_blocks.as_ref(), now, None);
            account
        })
        .collect()
}
fn select_unblocked(
    accounts: &[AccountSlot],
    options: &AffinityOptions<'_>,
    now: f64,
) -> Option<AccountSlot> {
    if let Some(pin) = accounts
        .iter()
        .find(|a| Some(a.name.as_str()) == options.pinned_account)
        && !is_blocked_for(pin, now, options.model)
    {
        return Some(pin.clone());
    }
    if let Some(preferred) = accounts
        .iter()
        .find(|a| Some(a.name.as_str()) == options.preferred_account)
        && !is_blocked_for(preferred, now, options.model)
    {
        return Some(preferred.clone());
    }
    apply_account_order(
        options.account_order,
        rendezvous_order(get_affinity_key(options), accounts),
    )
    .into_iter()
    .find(|a| !is_blocked_for(a, now, options.model))
}
fn unblock_at(account: &AccountSlot, now: f64, model: Option<&str>) -> Option<f64> {
    let account_until = account.blocked_until.filter(|until| *until > now);
    let model_until =
        crate::model_blocks::active_model_block_until(account.model_blocks.as_ref(), model, now);
    match (account_until, model_until) {
        (Some(a), Some(m)) => Some(a.max(m)),
        (a, m) => a.or(m),
    }
}
pub fn select_account(
    accounts: &[AccountSlot],
    options: &AffinityOptions<'_>,
    now: f64,
) -> Result<AccountSlot, AllAccountsBlockedError> {
    let mut candidates = accounts;
    let single: Vec<AccountSlot>;
    let mut unavailable_account = None;
    if options.account_fallback == Some(false) {
        let name = options
            .pinned_account
            .or(options.preferred_account)
            .map(str::to_owned)
            .or_else(|| {
                apply_account_order(
                    options.account_order,
                    rendezvous_order(get_affinity_key(options), accounts),
                )
                .first()
                .map(|a| a.name.clone())
            });
        single = accounts
            .iter()
            .filter(|a| Some(&a.name) == name.as_ref())
            .cloned()
            .collect();
        if let Some(name) = name {
            unavailable_account = Some(UnavailableAccount {
                configured: !single.is_empty(),
                pinned_by: options.pinned_account.and(options.pin_source),
                name,
            });
        }
        candidates = &single;
    }
    select_unblocked(candidates, options, now)
        .or_else(|| select_unblocked(&clear_expired_blocks(candidates, now), options, now))
        .ok_or_else(|| AllAccountsBlockedError {
            soonest_unblock_at: candidates
                .iter()
                .filter_map(|a| unblock_at(a, now, options.model))
                .min_by(f64::total_cmp),
            limited_model: candidates
                .iter()
                .any(|a| {
                    !is_account_blocked(a, now)
                        && crate::model_blocks::active_model_block_until(
                            a.model_blocks.as_ref(),
                            options.model,
                            now,
                        )
                        .is_some()
                })
                .then(|| options.model.map(str::to_owned))
                .flatten(),
            block_reason: candidates
                .iter()
                .all(|a| a.block_reason.as_deref() == Some("auth_error"))
                .then_some("auth_error"),
            unavailable_account,
        })
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::AccountSource;
    fn accounts() -> Vec<AccountSlot> {
        ["alpha", "bravo", "charlie"]
            .map(|name| AccountSlot {
                name: name.into(),
                display_name: None,
                refresh: String::new(),
                access: String::new(),
                expires: 0.0,
                source: AccountSource::Login,
                blocked_until: None,
                block_reason: None,
                ..Default::default()
            })
            .into()
    }
    #[test]
    fn mixed_blocks_are_not_classified_as_auth() {
        let mut pool = accounts();
        for account in &mut pool {
            account.blocked_until = Some(100.0);
        }
        pool[0].block_reason = Some("auth_error".into());
        assert_eq!(
            select_account(&pool, &Default::default(), 1.0)
                .unwrap_err()
                .block_reason,
            None
        );
        for account in &mut pool {
            account.block_reason = Some("auth_error".into());
        }
        assert_eq!(
            select_account(&pool, &Default::default(), 1.0)
                .unwrap_err()
                .block_reason,
            Some("auth_error")
        );
    }
    #[test]
    fn model_blocks_preferred_account_and_order_steer_selection() {
        let mut pool = accounts();
        pool[0].model_blocks = Some(
            [(
                "opus".to_owned(),
                crate::model_blocks::ModelBlock {
                    blocked_until: 500.0,
                },
            )]
            .into(),
        );
        let options = |model| AffinityOptions {
            pinned_account: Some("alpha"),
            model,
            ..Default::default()
        };
        assert_eq!(
            select_account(&pool, &options(Some("claude-sonnet-5")), 1.0)
                .unwrap()
                .name,
            "alpha"
        );
        assert_ne!(
            select_account(&pool, &options(Some("claude-opus-5")), 1.0)
                .unwrap()
                .name,
            "alpha"
        );
        let order = ["charlie".to_owned()];
        let ordered = AffinityOptions {
            session_id: Some("session-01"),
            account_order: Some(&order),
            ..Default::default()
        };
        assert_eq!(
            select_account(&pool, &ordered, 1.0).unwrap().name,
            "charlie"
        );
        let preferred = AffinityOptions {
            account_order: Some(&order),
            preferred_account: Some("bravo"),
            ..Default::default()
        };
        assert_eq!(
            select_account(&pool, &preferred, 1.0).unwrap().name,
            "bravo"
        );
        let single = vec![pool[0].clone()];
        let error = select_account(&single, &options(Some("claude-opus-5")), 1.0).unwrap_err();
        assert_eq!(error.limited_model.as_deref(), Some("claude-opus-5"));
        assert_eq!(error.soonest_unblock_at, Some(500.0));
        assert!(
            error
                .to_string()
                .contains("usage limit for model claude-opus-5")
        );
    }
    #[test]
    fn key_precedence() {
        assert_eq!(get_affinity_key(&Default::default()), DEFAULT_AFFINITY_KEY);
        assert_eq!(
            get_affinity_key(&AffinityOptions {
                affinity_key: Some("parent"),
                session_id: Some("session"),
                ..Default::default()
            }),
            "parent"
        );
        assert_eq!(
            get_affinity_key(&AffinityOptions {
                session_id: Some("session"),
                ..Default::default()
            }),
            "session"
        );
    }
    #[test]
    fn exact_hrw_winners() {
        let winners: Vec<_> = (1..=6)
            .map(|i| {
                let key = format!("session-{i:02}");
                select_account(
                    &accounts(),
                    &AffinityOptions {
                        session_id: Some(&key),
                        ..Default::default()
                    },
                    0.0,
                )
                .unwrap()
                .name
            })
            .collect();
        assert_eq!(
            winners,
            ["alpha", "alpha", "alpha", "alpha", "bravo", "bravo"]
        );
    }
    #[test]
    fn minimal_disruption() {
        let original = accounts();
        let mut expanded = original.clone();
        let mut delta = original[0].clone();
        delta.name = "delta".into();
        expanded.push(delta);
        let changed: Vec<_> = (1..=6)
            .filter(|i| {
                let key = format!("session-{i:02}");
                let options = AffinityOptions {
                    session_id: Some(&key),
                    ..Default::default()
                };
                select_account(&original, &options, 0.0).unwrap().name
                    != select_account(&expanded, &options, 0.0).unwrap().name
            })
            .collect();
        assert_eq!(changed, [5]);
    }
    #[test]
    fn ancillary_uses_originating_key() {
        let parent = select_account(
            &accounts(),
            &AffinityOptions {
                session_id: Some("parent-session"),
                ..Default::default()
            },
            0.0,
        )
        .unwrap();
        let child = select_account(
            &accounts(),
            &AffinityOptions {
                session_id: Some("isolated-summary"),
                affinity_key: Some("parent-session"),
                ..Default::default()
            },
            0.0,
        )
        .unwrap();
        assert_eq!(parent, child);
    }
    #[test]
    fn pin_and_unblocked_walk() {
        let mut pool = accounts();
        let options = AffinityOptions {
            session_id: Some("session-01"),
            pinned_account: Some("charlie"),
            ..Default::default()
        };
        assert_eq!(
            select_account(&pool, &options, 1000.0).unwrap().name,
            "charlie"
        );
        pool[0].blocked_until = Some(10000.0);
        assert_eq!(
            select_account(
                &pool,
                &AffinityOptions {
                    session_id: Some("session-01"),
                    ..Default::default()
                },
                1000.0
            )
            .unwrap()
            .name,
            "charlie"
        );
    }
    #[test]
    fn single_account_mode_reports_the_pin_source() {
        let mut pool = accounts();
        pool[0].blocked_until = Some(10000.0);
        pool[0].block_reason = Some("rate_limit".into());
        let options = |pinned: Option<&'static str>, source| AffinityOptions {
            pinned_account: pinned,
            pin_source: source,
            account_fallback: Some(false),
            ..Default::default()
        };
        let error = select_account(&pool, &options(Some("alpha"), Some(PinSource::Cli)), 1000.0)
            .unwrap_err();
        assert_eq!(
            error.unavailable_account,
            Some(UnavailableAccount {
                name: "alpha".into(),
                configured: true,
                pinned_by: Some(PinSource::Cli)
            })
        );
        let missing = select_account(
            &pool,
            &options(Some("gone"), Some(PinSource::Settings)),
            1000.0,
        )
        .unwrap_err();
        assert_eq!(
            missing.unavailable_account,
            Some(UnavailableAccount {
                name: "gone".into(),
                configured: false,
                pinned_by: Some(PinSource::Settings)
            })
        );
        let unpinned = select_account(&pool, &options(None, Some(PinSource::Cli)), 1000.0);
        if let Err(error) = unpinned {
            assert!(
                error
                    .unavailable_account
                    .is_some_and(|account| account.pinned_by.is_none())
            );
        }
        assert!(
            select_account(
                &pool,
                &AffinityOptions {
                    pinned_account: Some("alpha"),
                    ..Default::default()
                },
                1000.0
            )
            .is_ok()
        );
    }
    #[test]
    fn expired_typed_auth_block_clears_at_reprobe() {
        let mut pool = accounts();
        for a in &mut pool {
            a.blocked_until = Some(10000.0);
        }
        assert_eq!(
            select_account(&pool, &Default::default(), 1000.0)
                .unwrap_err()
                .soonest_unblock_at,
            Some(10000.0)
        );
        for a in &mut pool {
            a.block_reason = Some("auth_error".into());
        }
        // Upstream affinity.ts clears auth blocks once blockedUntil reaches the re-probe time.
        assert!(select_account(&pool, &Default::default(), 10000.0).is_ok());
    }
    #[test]
    fn auth_blocks_expire_at_the_reprobe_time() {
        let mut pool = accounts();
        pool[0].blocked_until = Some(100.0);
        pool[0].block_reason = Some("auth_error".into());
        assert!(
            select_account(
                &pool,
                &AffinityOptions {
                    pinned_account: Some("alpha"),
                    ..Default::default()
                },
                99.0
            )
            .unwrap()
            .name
                != "alpha"
        );
        assert_eq!(
            select_account(
                &pool,
                &AffinityOptions {
                    pinned_account: Some("alpha"),
                    ..Default::default()
                },
                100.0
            )
            .unwrap()
            .name,
            "alpha"
        );
        let mut legacy_account = pool[0].clone();
        legacy_account.blocked_until = None;
        let legacy = clear_expired_blocks(&[legacy_account], 0.0);
        assert_eq!(legacy[0].block_reason, None);
    }
}
