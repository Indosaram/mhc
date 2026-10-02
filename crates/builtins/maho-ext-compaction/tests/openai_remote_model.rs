use std::collections::BTreeMap;
use maho_ai::types::Model;
use maho_ext_compaction::openai_remote_model::*;
#[test]
fn live_codex_headers_include_the_running_kernel_and_native_architecture() {
    let m = model("chatgpt-subscription", "openai-codex-responses", "");
    let headers = create_live_openai_remote_compaction_headers(&m, Some("fixture"), &BTreeMap::new(), None).unwrap().unwrap();
    let release = std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap();
    assert_eq!(headers["user-agent"], format!("senpi (linux {}; {})", release.trim_end(), "x64"));
}
use serde_json::json;
fn model(provider:&str,api:&str,base:&str)->Model {serde_json::from_value(json!({"id":"m","name":"m","api":api,"provider":provider,"baseUrl":base,"reasoning":false,"input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":200000,"maxTokens":4000})).expect("model")}
#[test] fn remote_identity_is_provider_scoped() {assert!(is_openai_remote_compaction_model(Some(&model("openai","openai-responses",""))));assert!(!is_openai_remote_compaction_model(Some(&model("foreign","openai-responses",""))));assert!(!is_openai_remote_compaction_model(None));}
#[test] fn codex_requires_trusted_or_loopback_endpoint() {for (base,expected) in [("https://chatgpt.com/backend-api",true),("http://localhost:8080",true),("http://[::1]:8080",true),("https://evil.test",false),("http://chatgpt.com",false)] {assert_eq!(is_openai_remote_compaction_model(Some(&model("chatgpt-subscription","openai-codex-responses",base))),expected);}}
#[test] fn endpoint_preserves_base_path() {assert_eq!(openai_remote_compaction_endpoint_url(&model("openai","openai-responses","https://api.openai.com/v1")).expect("url"),"https://api.openai.com/v1/responses/compact");}
#[test] fn origin_excludes_volatile_headers_and_normalizes_endpoint() {let m=model("openai","openai-responses","https://user:pass@api.openai.com/v1///?q=1#f");let mut headers=BTreeMap::from([("authorization".into(),"Bearer fixture".into())]);let first=openai_remote_compaction_origin(&m,&headers).expect("origin");headers.insert("X-Request-ID".into(),"request".into());assert_eq!(openai_remote_compaction_origin(&m,&headers),Some(first.clone()));assert_eq!(first.endpoint,"https://api.openai.com/v1");}
#[test] fn codex_fingerprint_ignores_rotating_authorization() {let m=model("chatgpt-subscription","openai-codex-responses","");let mut headers=BTreeMap::from([("authorization".into(),"Bearer first".into()),("chatgpt-account-id".into(),"fixture-account".into())]);let first=openai_remote_compaction_origin(&m,&headers);headers.insert("authorization".into(),"Bearer second".into());assert_eq!(openai_remote_compaction_origin(&m,&headers),first);headers.remove("chatgpt-account-id");assert!(openai_remote_compaction_origin(&m,&headers).is_none());}
#[test] fn headers_require_authorization_and_allow_hook_deletion() {let m=model("openai","openai-responses","");assert!(create_openai_remote_compaction_headers(&m,None,&BTreeMap::new(),None,"").is_none());let headers=create_openai_remote_compaction_headers(&m,Some("fixture"),&BTreeMap::new(),None,"").expect("headers");assert_eq!(headers["content-type"],"application/json");assert_eq!(headers["authorization"],"Bearer fixture");}
