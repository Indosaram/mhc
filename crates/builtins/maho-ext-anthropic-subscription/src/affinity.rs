use crate::accounts::AccountSlot;
use sha2::{Digest,Sha256};
pub const DEFAULT_AFFINITY_KEY:&str="claude-sdk-oauth-default";
#[derive(Default)]
pub struct AffinityOptions<'a> { pub affinity_key:Option<&'a str>,pub session_id:Option<&'a str>,pub pinned_account:Option<&'a str> }
#[derive(Debug,PartialEq)]
pub struct AllAccountsBlockedError { pub soonest_unblock_at:Option<f64>,pub block_reason:Option<&'static str> }
impl std::fmt::Display for AllAccountsBlockedError {
    fn fmt(&self,f:&mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.soonest_unblock_at {
            None=>f.write_str("All Anthropic Subscription accounts are blocked until re-login."),
            Some(at)=>{
                let date=chrono::DateTime::from_timestamp_millis(at as i64).ok_or(std::fmt::Error)?;
                write!(f,"All Anthropic Subscription accounts are blocked until {}.",date.to_rfc3339_opts(chrono::SecondsFormat::Millis,true))
            }
        }
    }
}
impl std::error::Error for AllAccountsBlockedError {}
pub fn get_affinity_key<'a>(options:&AffinityOptions<'a>) -> &'a str { options.affinity_key.or(options.session_id).unwrap_or(DEFAULT_AFFINITY_KEY) }
fn score(key:&str,name:&str) -> u64 {
    let mut hash=Sha256::new();hash.update(key.as_bytes());hash.update([0]);hash.update(name.as_bytes());let digest=hash.finalize();
    u64::from_be_bytes([digest[0],digest[1],digest[2],digest[3],digest[4],digest[5],digest[6],digest[7]])
}
pub fn rendezvous_order(key:&str,accounts:&[AccountSlot]) -> Vec<AccountSlot> {
    let mut ranked:Vec<_>=accounts.iter().map(|account| (score(key,&account.name),account)).collect();
    ranked.sort_by_key(|entry| std::cmp::Reverse(entry.0));ranked.into_iter().map(|(_,account)| account.clone()).collect()
}
fn is_blocked(account:&AccountSlot,now:f64) -> bool {
    account.block_reason.as_deref()==Some("auth_error") || account.blocked_until.is_some_and(|until| until>now)
}
pub fn clear_expired_blocks(accounts:&[AccountSlot],now:f64) -> Vec<AccountSlot> {
    accounts.iter().map(|account| {
        let mut account=account.clone();
        if account.block_reason.as_deref()!=Some("auth_error") && account.blocked_until.is_some_and(|until| until<=now) {
            account.blocked_until=None;account.block_reason=None;
        } account
    }).collect()
}
fn select_unblocked(accounts:&[AccountSlot],options:&AffinityOptions<'_>,now:f64) -> Option<AccountSlot> {
    if let Some(pin)=accounts.iter().find(|a| Some(a.name.as_str())==options.pinned_account) && !is_blocked(pin,now) { return Some(pin.clone()); }
    rendezvous_order(get_affinity_key(options),accounts).into_iter().find(|a| !is_blocked(a,now))
}
pub fn select_account(accounts:&[AccountSlot],options:&AffinityOptions<'_>,now:f64) -> Result<AccountSlot,AllAccountsBlockedError> {
    select_unblocked(accounts,options,now).or_else(|| select_unblocked(&clear_expired_blocks(accounts,now),options,now))
        .ok_or_else(|| AllAccountsBlockedError {
            soonest_unblock_at:accounts.iter().filter_map(|a| a.blocked_until).filter(|until| *until>now).min_by(f64::total_cmp),
            block_reason:accounts.iter().any(|a| a.block_reason.as_deref()==Some("auth_error")).then_some("auth_error"),
        })
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::AccountSource;
    fn accounts() -> Vec<AccountSlot> { ["alpha","bravo","charlie"].map(|name| AccountSlot {
        name:name.into(),display_name:None,refresh:String::new(),access:String::new(),expires:0.0,source:AccountSource::Login,
        blocked_until:None,block_reason:None }).into() }
    #[test]
    fn key_precedence() {
        assert_eq!(get_affinity_key(&Default::default()),DEFAULT_AFFINITY_KEY);
        assert_eq!(get_affinity_key(&AffinityOptions { affinity_key:Some("parent"),session_id:Some("session"),..Default::default() }),"parent");
        assert_eq!(get_affinity_key(&AffinityOptions { session_id:Some("session"),..Default::default() }),"session");
    }
    #[test]
    fn exact_hrw_winners() {
        let winners:Vec<_>=(1..=6).map(|i| { let key=format!("session-{i:02}");select_account(&accounts(),&AffinityOptions { session_id:Some(&key),..Default::default() },0.0).unwrap().name }).collect();
        assert_eq!(winners,["alpha","alpha","alpha","alpha","bravo","bravo"]);
    }
    #[test]
    fn minimal_disruption() {
        let original=accounts();let mut expanded=original.clone();let mut delta=original[0].clone();delta.name="delta".into();expanded.push(delta);
        let changed:Vec<_>=(1..=6).filter(|i| { let key=format!("session-{i:02}");let options=AffinityOptions { session_id:Some(&key),..Default::default() };
            select_account(&original,&options,0.0).unwrap().name!=select_account(&expanded,&options,0.0).unwrap().name }).collect();assert_eq!(changed,[5]);
    }
    #[test]
    fn ancillary_uses_originating_key() {
        let parent=select_account(&accounts(),&AffinityOptions { session_id:Some("parent-session"),..Default::default() },0.0).unwrap();
        let child=select_account(&accounts(),&AffinityOptions { session_id:Some("isolated-summary"),affinity_key:Some("parent-session"),..Default::default() },0.0).unwrap();
        assert_eq!(parent,child);
    }
    #[test]
    fn pin_and_unblocked_walk() {
        let mut pool=accounts();let options=AffinityOptions { session_id:Some("session-01"),pinned_account:Some("charlie"),..Default::default() };
        assert_eq!(select_account(&pool,&options,1000.0).unwrap().name,"charlie");
        pool[0].blocked_until=Some(10000.0);
        assert_eq!(select_account(&pool,&AffinityOptions { session_id:Some("session-01"),..Default::default() },1000.0).unwrap().name,"charlie");
    }
    #[test]
    fn typed_blocked_pool_auth_never_expires() {
        let mut pool=accounts();for a in &mut pool { a.blocked_until=Some(10000.0); }
        assert_eq!(select_account(&pool,&Default::default(),1000.0).unwrap_err().soonest_unblock_at,Some(10000.0));
        for a in &mut pool { a.block_reason=Some("auth_error".into()); }
        assert_eq!(select_account(&pool,&Default::default(),1000000.0).unwrap_err().block_reason,Some("auth_error"));
    }
}
