pub const MINIMUM_KNOWN_GOOD_VERSION:&str="2026.08.11";
pub fn parse_version(version:&str)->Option<[u32;3]> {
    let regex=regex::Regex::new(r"^v?(\d{1,4})\.(\d{1,2})\.(\d{1,2})").expect("constant version regex");
    let capture=regex.captures(version.trim())?;Some([capture[1].parse().ok()?,capture[2].parse().ok()?,capture[3].parse().ok()?])
}
pub fn compare_versions(version:&str,floor:&str)->Option<std::cmp::Ordering> {Some(parse_version(version)?.cmp(&parse_version(floor)?))}
#[derive(serde::Serialize)]
#[serde(rename_all="camelCase")]
pub struct BlockWindow {pub account:String,pub reason:&'static str,pub blocked_until:Option<f64>}
pub fn block_windows(accounts:&[crate::accounts::CursorCliAccountSlot],now:f64)->Vec<BlockWindow> {
    accounts.iter().filter_map(|a| {
        if a.block_reason==Some(crate::accounts::BlockReason::AuthError) {Some(BlockWindow {account:a.name.clone(),reason:"auth_error",blocked_until:None})}
        else {a.blocked_until.filter(|until|*until>now).map(|until|BlockWindow {account:a.name.clone(),reason:"rate_limit",blocked_until:Some(until)})}
    }).collect()
}
#[derive(Default)]
pub struct GenerationGuard {reason:Option<String>}
impl GenerationGuard {
    pub fn retire(&mut self,reason:&str) {if self.reason.is_none() {self.reason=Some(format!("extension generation retired ({reason})"));}}
    pub fn is_retired(&self)->bool {self.reason.is_some()}
    pub fn run_fenced<T>(&self,context_retired:bool,work:impl FnOnce()->T)->Result<T,String> {
        if let Some(reason)=&self.reason {return Err(reason.clone());}
        if context_retired {return Err("extension context retired by reload".into());}Ok(work())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn version_floor_and_unknown() {assert_eq!(compare_versions(" v2026.08.10-hash ",MINIMUM_KNOWN_GOOD_VERSION),Some(std::cmp::Ordering::Less));assert_eq!(compare_versions("2026.08.11",MINIMUM_KNOWN_GOOD_VERSION),Some(std::cmp::Ordering::Equal));assert!(parse_version("unknown").is_none());}
    #[test]
    fn retirement_is_idempotent_and_fences_work() {let mut guard=GenerationGuard::default();assert_eq!(guard.run_fenced(false,||7),Ok(7));guard.retire("reload");guard.retire("shutdown");assert!(guard.run_fenced(false,||panic!("retired work must not run")).expect_err("retired").contains("reload"));}
    #[test]
    fn version_parsing_rejects_malformed_and_accepts_prefixed_prerelease() {
        assert_eq!(parse_version(" v2026.08.11-beta.1 "),Some([2026,8,11]));
        assert!(parse_version("2026.08").is_none());assert!(parse_version("v2026.x.11").is_none());
    }
    #[test]
    fn block_windows_report_auth_until_re_login_and_future_rate_limits() {
        let accounts=["auth","future","expired"].map(|name|crate::accounts::CursorCliAccountSlot {
            name:name.into(),display_name:None,access:String::new(),refresh:String::new(),expires:0.0,source:crate::accounts::AccountSource::Login,
            blocked_until:match name {"future"=>Some(20.0),"expired"=>Some(5.0),_=>None},
            block_reason:match name {"auth"=>Some(crate::accounts::BlockReason::AuthError),"future"|"expired"=>Some(crate::accounts::BlockReason::RateLimit),_=>None},
        });
        let windows=block_windows(&accounts,10.0);
        assert_eq!(windows.len(),2);assert_eq!((windows[0].account.as_str(),windows[0].reason,windows[0].blocked_until),("auth","auth_error",None));
        assert_eq!((windows[1].account.as_str(),windows[1].reason,windows[1].blocked_until),("future","rate_limit",Some(20.0)));
    }
}
