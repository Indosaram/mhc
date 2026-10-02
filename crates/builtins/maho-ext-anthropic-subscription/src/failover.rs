use crate::{accounts::{AccountSlot,AccountSource},errors::{SdkErrorClassification,SdkErrorKind}};
use maho_ai::auth::types::{Credential,CredentialStore};
use serde_json::Value;
pub const MAX_RATE_LIMIT_BLOCK_MS:f64=48.0*60.0*60.0*1000.0;
pub const DEFAULT_RATE_LIMIT_BLOCK_MS:f64=60000.0;
pub const TURN_RETRY_SUPPRESSION_PREFIX:&str="senpi:no-turn-retry:";
pub type AttemptFuture<'a>=std::pin::Pin<Box<dyn std::future::Future<Output=Result<Option<Value>,Value>>+Send+'a>>;
pub trait AttemptMessages:Send {fn next(&mut self)->AttemptFuture<'_>;}
pub type AttemptFactoryFuture<'a>=std::pin::Pin<Box<dyn std::future::Future<Output=Result<Box<dyn AttemptMessages>,Value>>+Send+'a>>;
#[derive(Debug)]
pub struct ClassifiedSdkError {pub classification:SdkErrorClassification,pub original:Value,pub suppress_turn_retry:bool}
impl std::fmt::Display for ClassifiedSdkError {
    fn fmt(&self,formatter:&mut std::fmt::Formatter<'_>)->std::fmt::Result {if self.suppress_turn_retry {formatter.write_str(TURN_RETRY_SUPPRESSION_PREFIX)?;}
        if let Some(text)=self.original.as_str().or_else(||self.original["message"].as_str()) {formatter.write_str(text)}else {write!(formatter,"{}",self.original)}}
}
impl std::error::Error for ClassifiedSdkError {}
pub struct FailoverEvent {pub account:AccountSlot,pub next_account:Option<AccountSlot>,pub classification:SdkErrorClassification,pub attempt:usize,pub visible_delta_emitted:bool}
pub struct FailoverOptions<'a> {pub accounts:&'a [AccountSlot],pub store:&'a dyn CredentialStore,pub provider:&'a str,pub now:&'a dyn Fn()->f64,pub base_block_ms:f64}
pub trait FailoverBoundary {
    fn select(&mut self,accounts:&[AccountSlot])->anyhow::Result<AccountSlot>;
    fn run_attempt(&mut self,account:AccountSlot)->AttemptFactoryFuture<'_>;
    fn emit(&mut self,event:Value);
    fn error_from_event(&self,event:&Value)->Option<Value> {if event["type"]=="error" {event.get("error").cloned()}else {None}}
    fn is_visible_delta(&self,event:&Value)->bool {event["type"].as_str().is_some_and(|kind|["text","thinking","toolcall"].iter().any(|prefix|["start","delta","end"].iter().any(|suffix|kind==format!("{prefix}_{suffix}"))))}
    fn on_failover(&mut self,event:FailoverEvent)->std::pin::Pin<Box<dyn std::future::Future<Output=anyhow::Result<()>>+Send+'_>>;
}
pub async fn run_failover(options:FailoverOptions<'_>,boundary:&mut impl FailoverBoundary)->anyhow::Result<()> {
    let mut accounts=crate::affinity::clear_expired_blocks(options.accounts,(options.now)());let mut last_error=None;
    for attempt in 0..accounts.len() {
        let account=boundary.select(&accounts)?;let mut visible=false;
        let failure=match boundary.run_attempt(account.clone()).await {
            Err(error)=>error,
            Ok(mut messages)=>loop {match messages.next().await {
                Ok(None)=>return Ok(()),Err(error)=>break error,
                Ok(Some(event))=>{if let Some(error)=boundary.error_from_event(&event) {break error;}visible|=boundary.is_visible_delta(&event);boundary.emit(event);}
            }},
        };
        let classification=crate::errors::classify_sdk_error(&failure);let classified=ClassifiedSdkError {classification,original:failure.clone(),suppress_turn_retry:visible};if !classification.retryable {return Err(classified.into());}
        let blocked=blocked_account(&account,classification,(options.now)(),attempt as i32,options.base_block_ms,&failure);for current in &mut accounts {if current.name==blocked.name {*current=blocked.clone();}}
        persist_block(options.store,options.provider,blocked.clone()).await?;
        let selection=if !visible&&attempt+1<accounts.len() {boundary.select(&accounts).map(Some)}else {Ok(None)};
        boundary.on_failover(FailoverEvent {account:blocked,next_account:selection.as_ref().ok().cloned().flatten(),classification,attempt:attempt+1,visible_delta_emitted:visible}).await?;selection?;
        if visible {return Err(classified.into());}last_error=Some(classified);
    }
    Err(last_error.map(anyhow::Error::from).unwrap_or_else(||anyhow::anyhow!("Anthropic Subscription failover exhausted without an attempt")))
}
pub fn retry_after_ms(error:&Value)->Option<f64> {
    if let Some(explicit)=error["retryAfterMs"].as_f64().filter(|v|v.is_finite()&&*v>0.0) {return Some(explicit.ceil());}
    let text=error.as_str().or_else(||error["message"].as_str()).map(str::to_owned).unwrap_or_else(||error.to_string());
    for (pattern,multiplier) in [(r"(?i)\bretry[-_ ]?after[-_ ]?ms\s*[:=]\s*(\d+(?:\.\d+)?)",1.0),(r"(?i)\bretry[-_ ]?after\s*[:=]\s*(\d+(?:\.\d+)?)",1000.0)] {
        if let Some(value)=regex::Regex::new(pattern).ok().and_then(|regex|regex.captures(&text).and_then(|capture|capture[1].parse::<f64>().ok())) {return Some((value*multiplier).ceil());}
    }None
}
fn kind_name(kind:SdkErrorKind)->&'static str {match kind {SdkErrorKind::RateLimit=>"rate_limit",SdkErrorKind::Overloaded=>"overloaded",SdkErrorKind::AuthError=>"auth_error",SdkErrorKind::Billing=>"billing",SdkErrorKind::OrgNotAllowed=>"org_not_allowed",SdkErrorKind::Entitlement=>"entitlement",SdkErrorKind::Other=>"other"}}
pub fn blocked_account(account:&AccountSlot,classification:SdkErrorClassification,now:f64,attempt:i32,base_block_ms:f64,error:&Value)->AccountSlot {
    let mut blocked=account.clone();blocked.block_reason=Some(kind_name(classification.kind).into());
    blocked.blocked_until=if classification.kind==SdkErrorKind::AuthError {None}else {let fallback=(base_block_ms*2_f64.powi(attempt)).min(MAX_RATE_LIMIT_BLOCK_MS);Some(now+retry_after_ms(error).unwrap_or(fallback).min(MAX_RATE_LIMIT_BLOCK_MS))};blocked
}
pub async fn persist_block(store:&dyn CredentialStore,provider:&str,account:AccountSlot)->anyhow::Result<()> {
    store.modify(provider,Box::new(move |current|Box::pin(async move {
        let Some(Credential::OAuth(mut credential))=current else {return Ok(current);};
        let state=serde_json::json!({"blockedUntil":account.blocked_until,"blockReason":account.block_reason});
        if account.source==AccountSource::Env {
            let states=credential.extra.entry("slotState").or_insert_with(||serde_json::json!({}));states[&account.name]=state;
        }else if let Some(accounts)=credential.extra.get_mut("accounts").and_then(Value::as_array_mut) {
            for existing in accounts {if existing["name"]==account.name {let object=existing.as_object_mut().ok_or_else(||anyhow::anyhow!("account must be object"))?;if let Some(expiry)=account.blocked_until {object.insert("blockedUntil".into(),serde_json::json!(expiry));}else {object.remove("blockedUntil");}object.insert("blockReason".into(),serde_json::json!(account.block_reason));}}
        }
        Ok(Some(Credential::OAuth(credential)))
    })),None).await?;Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    struct Messages(std::collections::VecDeque<Result<Value,Value>>);
    impl AttemptMessages for Messages {fn next(&mut self)->AttemptFuture<'_> {Box::pin(async {self.0.pop_front().transpose()})}}
    struct Boundary {streams:std::collections::VecDeque<Vec<Result<Value,Value>>>,emitted:Vec<Value>,attempts:Vec<String>,failovers:Vec<FailoverEvent>}
    impl FailoverBoundary for Boundary {
        fn select(&mut self,accounts:&[AccountSlot])->anyhow::Result<AccountSlot> {Ok(crate::affinity::select_account(accounts,&crate::affinity::AffinityOptions {pinned_account:Some("a"),..Default::default()},0.0)?)}
        fn run_attempt(&mut self,account:AccountSlot)->AttemptFactoryFuture<'_> {self.attempts.push(account.name);let messages=self.streams.pop_front().expect("attempt stream");Box::pin(async move {Ok(Box::new(Messages(messages.into())) as Box<dyn AttemptMessages>)})}
        fn emit(&mut self,event:Value) {self.emitted.push(event);}
        fn on_failover(&mut self,event:FailoverEvent)->std::pin::Pin<Box<dyn std::future::Future<Output=anyhow::Result<()>>+Send+'_>> {self.failovers.push(event);Box::pin(async {Ok(())})}
    }
    #[tokio::test]
    async fn retries_only_before_visible_delta_and_persists_failed_account() {
        use maho_ai::auth::credential_store::InMemoryCredentialStore;
        for visible in [false,true] {
            let accounts:Vec<_>=["a","b"].into_iter().map(|name|AccountSlot {name:name.into(),display_name:None,refresh:String::new(),access:String::new(),expires:0.0,source:AccountSource::Login,blocked_until:None,block_reason:None}).collect();let store=InMemoryCredentialStore::new();let credential=crate::accounts::empty_credential().with_extra("accounts",serde_json::json!(accounts));store.modify("provider",Box::new(move |_|Box::pin(async move {Ok(Some(Credential::OAuth(credential)))})),None).await.expect("seed");
            let mut first=Vec::new();if visible {first.push(Ok(serde_json::json!({"type":"text_delta","delta":"partial"})));}first.push(Err(serde_json::json!("rate_limit")));let mut boundary=Boundary {streams:vec![first,vec![Ok(serde_json::json!({"type":"text_delta","delta":"success"}))]].into(),emitted:Vec::new(),attempts:Vec::new(),failovers:Vec::new()};let result=run_failover(FailoverOptions {accounts:&accounts,store:&store,provider:"provider",now:&||0.0,base_block_ms:DEFAULT_RATE_LIMIT_BLOCK_MS},&mut boundary).await;
            assert_eq!(boundary.attempts.len(),if visible {1}else {2});assert_eq!(boundary.failovers.len(),1);assert_eq!(boundary.failovers[0].visible_delta_emitted,visible);if visible {assert!(result.expect_err("partial failure").downcast_ref::<ClassifiedSdkError>().expect("classified").suppress_turn_retry);}else {result.expect("failover success");assert_eq!(boundary.attempts,["a","b"]);}
            let credential=store.read("provider",None).await.expect("read").expect("credential").into_oauth().expect("oauth");assert_eq!(credential.extra["accounts"][0]["blockedUntil"],DEFAULT_RATE_LIMIT_BLOCK_MS);
        }
    }
    #[test]
    fn explicit_backoff_clamps_and_auth_has_no_expiry() {
        let slot=AccountSlot {name:"env".into(),display_name:None,refresh:String::new(),access:String::new(),expires:0.0,source:AccountSource::Env,blocked_until:Some(42.0),block_reason:None};
        let classification=SdkErrorClassification {kind:SdkErrorKind::RateLimit,retryable:true};assert_eq!(retry_after_ms(&serde_json::json!("retry-after: 1.25")),Some(1250.0));assert_eq!(retry_after_ms(&serde_json::json!({"retryAfterMs":2.1})),Some(3.0));
        assert_eq!(blocked_account(&slot,classification,10.0,0,DEFAULT_RATE_LIMIT_BLOCK_MS,&serde_json::json!({"retryAfterMs":1e12})).blocked_until,Some(10.0+MAX_RATE_LIMIT_BLOCK_MS));assert_eq!(blocked_account(&slot,SdkErrorClassification {kind:SdkErrorKind::AuthError,retryable:true},10.0,0,DEFAULT_RATE_LIMIT_BLOCK_MS,&Value::Null).blocked_until,None);
    }
    #[tokio::test]
    async fn env_block_persistence_does_not_copy_token_fields() {
        use crate::accounts::empty_credential;
        use maho_ai::auth::credential_store::InMemoryCredentialStore;
        let store=InMemoryCredentialStore::new();store.modify("provider",Box::new(|_|Box::pin(async {Ok(Some(Credential::OAuth(empty_credential())))})),None).await.expect("seed");let account=AccountSlot {name:"env".into(),display_name:None,refresh:"not-stored".into(),access:"not-stored".into(),expires:0.0,source:AccountSource::Env,blocked_until:Some(123.0),block_reason:Some("rate_limit".into())};persist_block(&store,"provider",account).await.expect("block");let credential=store.read("provider",None).await.expect("read").expect("credential").into_oauth().expect("oauth");assert_eq!(credential.extra["slotState"]["env"]["blockedUntil"],123.0);assert!(credential.extra["slotState"]["env"].get("access").is_none());
    }
}
