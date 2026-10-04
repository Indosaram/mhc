use crate::accounts::{BlockReason, CursorCliAccountSlot};
use sha2::{Digest, Sha256};

pub const DEFAULT_CURSOR_AFFINITY_KEY: &str = "cursor-cli-oauth-default";

#[derive(Default)]
pub struct CursorAffinityOptions<'a> {
    pub affinity_key: Option<&'a str>,
    pub session_id: Option<&'a str>,
    pub pinned_account: Option<&'a str>,
}

#[derive(Debug, PartialEq)]
pub struct AllCursorAccountsBlockedError {
    pub soonest_unblock_at: Option<f64>,
}
impl std::fmt::Display for AllCursorAccountsBlockedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.soonest_unblock_at {
            None => f.write_str("All Cursor CLI OAuth accounts are blocked until re-login."),
            Some(at) => {
                let date = chrono::DateTime::from_timestamp_millis(at as i64).ok_or(std::fmt::Error)?;
                write!(f, "All Cursor CLI OAuth accounts are blocked until {}.", date.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
            }
        }
    }
}
impl std::error::Error for AllCursorAccountsBlockedError {}

pub fn get_affinity_key<'a>(options: &CursorAffinityOptions<'a>) -> &'a str {
    options.affinity_key.or(options.session_id).unwrap_or(DEFAULT_CURSOR_AFFINITY_KEY)
}

fn score(key: &str, name: &str) -> u64 {
    let mut hash = Sha256::new();
    hash.update(key.as_bytes()); hash.update([0]); hash.update(name.as_bytes());
    let digest = hash.finalize();
    u64::from_be_bytes([digest[0], digest[1], digest[2], digest[3], digest[4], digest[5], digest[6], digest[7]])
}

pub fn rendezvous_order(key: &str, accounts: &[CursorCliAccountSlot]) -> Vec<CursorCliAccountSlot> {
    let mut ranked: Vec<_> = accounts.iter().map(|a| (score(key, &a.name), a)).collect();
    ranked.sort_by_key(|entry| std::cmp::Reverse(entry.0));
    ranked.into_iter().map(|(_, a)| a.clone()).collect()
}

fn is_blocked(account: &CursorCliAccountSlot, now: f64) -> bool {
    account.block_reason == Some(BlockReason::AuthError) || account.blocked_until.is_some_and(|until| until > now)
}

pub fn clear_expired_blocks(accounts: &[CursorCliAccountSlot], now: f64) -> Vec<CursorCliAccountSlot> {
    accounts.iter().map(|a| {
        let mut account = a.clone();
        if account.block_reason != Some(BlockReason::AuthError) && account.blocked_until.is_some_and(|until| until <= now) {
            account.blocked_until = None; account.block_reason = None;
        }
        account
    }).collect()
}

fn select_unblocked(accounts: &[CursorCliAccountSlot], options: &CursorAffinityOptions<'_>, now: f64) -> Option<CursorCliAccountSlot> {
    if let Some(pinned) = accounts.iter().find(|a| Some(a.name.as_str()) == options.pinned_account)
        && !is_blocked(pinned, now) { return Some(pinned.clone()); }
    rendezvous_order(get_affinity_key(options), accounts).into_iter().find(|a| !is_blocked(a, now))
}

pub fn select_account(accounts: &[CursorCliAccountSlot], options: &CursorAffinityOptions<'_>, now: f64) -> Result<CursorCliAccountSlot, AllCursorAccountsBlockedError> {
    select_unblocked(accounts, options, now)
        .or_else(|| select_unblocked(&clear_expired_blocks(accounts, now), options, now))
        .ok_or_else(|| AllCursorAccountsBlockedError {
            soonest_unblock_at: accounts.iter().filter_map(|a| a.blocked_until).filter(|until| *until > now).min_by(f64::total_cmp),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn blocked_error_formats_numeric_timestamp() {
        let error=AllCursorAccountsBlockedError {soonest_unblock_at:Some(4000.0)};
        let mut output=String::new();assert!(std::fmt::write(&mut output,format_args!("{error}")).is_ok());
        assert!(output.contains("1970-01-01T00:00:04.000Z"));
    }
    use crate::accounts::AccountSource;
    fn accounts() -> Vec<CursorCliAccountSlot> {
        ["alpha", "bravo", "charlie"].map(|name| CursorCliAccountSlot { name: name.into(), display_name: None,
            access: String::new(), refresh: String::new(), expires: 0.0, source: AccountSource::Login,
            blocked_until: None, block_reason: None }).into()
    }
    #[test]
    fn stable_hrw_order() {
        let ranked = rendezvous_order("session-01", &accounts());
        assert_eq!(ranked.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(), ["alpha", "charlie", "bravo"]);
    }
    #[test]
    fn only_new_winner_moves() {
        let pool = accounts(); let mut expanded = pool.clone();
        let mut delta = pool[0].clone(); delta.name = "delta".into(); expanded.push(delta);
        let changed: Vec<_> = (1..=6).filter(|i| {
            let key = format!("session-{i:02}"); let options = CursorAffinityOptions { session_id: Some(&key), ..Default::default() };
            select_account(&pool, &options, 0.0).unwrap().name != select_account(&expanded, &options, 0.0).unwrap().name
        }).collect(); assert_eq!(changed, [5]);
    }
    #[test]
    fn available_pin_wins() {
        let options = CursorAffinityOptions { session_id: Some("session-01"), pinned_account: Some("charlie"), ..Default::default() };
        assert_eq!(select_account(&accounts(), &options, 1000.0).unwrap().name, "charlie");
    }
    #[test]
    fn blocked_pin_falls_through() {
        let mut pool = accounts(); pool[2].blocked_until = Some(10000.0); pool[2].block_reason = Some(BlockReason::RateLimit);
        let options = CursorAffinityOptions { session_id: Some("session-01"), pinned_account: Some("charlie"), ..Default::default() };
        assert_eq!(select_account(&pool, &options, 1000.0).unwrap().name, "alpha");
    }
    #[test]
    fn elapsed_blocks_clear_except_auth() {
        let mut pool = accounts(); pool[0].blocked_until = Some(999.0); pool[0].block_reason = Some(BlockReason::RateLimit);
        pool[1].blocked_until = Some(999.0); pool[1].block_reason = Some(BlockReason::AuthError);
        let cleared = clear_expired_blocks(&pool, 1000.0);
        assert_eq!(cleared[0].blocked_until, None); assert_eq!(cleared[0].block_reason, None); assert_eq!(cleared[1], pool[1]);
    }
    #[test]
    fn blocked_pool_has_soonest_time() {
        let mut pool = accounts(); pool[0].blocked_until = Some(9000.0); pool[1].blocked_until = Some(4000.0);
        pool[2].block_reason = Some(BlockReason::AuthError);
        let error = select_account(&pool, &Default::default(), 1000.0).unwrap_err();
        assert_eq!(error.soonest_unblock_at, Some(4000.0));
    }
    #[test]
    fn empty_and_auth_blocked_have_no_unblock_time() {
        assert_eq!(select_account(&[], &Default::default(), 1000.0).unwrap_err().soonest_unblock_at, None);
        let mut pool = accounts(); for a in &mut pool { a.block_reason = Some(BlockReason::AuthError); }
        assert_eq!(select_account(&pool, &Default::default(), 1000.0).unwrap_err().soonest_unblock_at, None);
    }
    #[test]
    fn affinity_key_precedence() {
        assert_eq!(get_affinity_key(&Default::default()), DEFAULT_CURSOR_AFFINITY_KEY);
        let options = CursorAffinityOptions { affinity_key: Some("explicit"), session_id: Some("session"), pinned_account: None };
        assert_eq!(get_affinity_key(&options), "explicit");
    }
}
