use crate::{accounts::{AccountSlot,AccountSource},errors::{SdkErrorClassification,SdkErrorKind}};
use maho_ai::auth::types::{Credential,CredentialStore};
use serde_json::Value;
pub const MAX_RATE_LIMIT_BLOCK_MS:f64=48.0*60.0*60.0*1000.0;
pub const DEFAULT_RATE_LIMIT_BLOCK_MS:f64=60000.0;
pub const TURN_RETRY_SUPPRESSION_PREFIX:&str="senpi:no-turn-retry:";
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
