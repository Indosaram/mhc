use maho_ext_anthropic_web_search::*;
use maho_ext_api::Model;
use serde_json::{json,Value};
fn model(api:&str,url:&str,override_value:Option<bool>)->Model {
 let mut value=json!({"id":"test","name":"Test","api":api,"provider":"anthropic","baseUrl":url,"reasoning":false,"input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":1000,"maxTokens":100});
 if let Some(v)=override_value { value["compat"]=json!({"supportsWebSearch":v}); }
 serde_json::from_value(value).expect("valid model fixture")
}
fn apply(m:&Model,p:&Value,enabled:bool)->Value { add_anthropic_web_search_to_payload(Some(m),p,&SearchOptions{enabled,..Default::default()}) }
#[test]
fn non_anthropic() { let m=model("openai-responses","https://api.openai.com",None); let p=json!({"tools":[{"name":"web_search"}]}); assert_eq!(apply(&m,&p,true),p); }

#[test]
fn injects() { let r=apply(&model("anthropic-messages","https://api.anthropic.com",None),&json!({"tools":[{"name":"other"}]}),true); assert_eq!(r["tools"][1],json!({"type":"web_search_20250305","name":"web_search","max_uses":8})); }

#[test]
fn preserves_version() { let p=json!({"tools":[{"type":"web_search_20260209","name":"web_search","max_uses":3}]}); assert_eq!(apply(&model("anthropic-messages","https://api.anthropic.com",None),&p,true),p); }

#[test]
fn replaces_function() { let r=apply(&model("anthropic-messages","https://api.anthropic.com",None),&json!({"tools":[{"name":"web_search"}]}),true); assert_eq!(r["tools"].as_array().unwrap().len(),1); assert_eq!(r["tools"][0]["type"],"web_search_20250305"); }

#[test]
fn other_api_preserves_function() { let m=model("openai-completions","",None); let p=json!({"tools":[{"name":"web_search"}]}); assert_eq!(apply(&m,&p,true),p); }

#[test]
fn domain_filters() { let m=model("anthropic-messages","https://api.anthropic.com",None); let o=SearchOptions{enabled:true,allowed_domains:Some(" docs.anthropic.com, example.com ".into()),blocked_domains:Some("spam.example, ads.example".into())}; let r=add_anthropic_web_search_to_payload(Some(&m),&json!({}),&o); assert_eq!(r["tools"][0]["allowed_domains"],json!(["docs.anthropic.com","example.com"])); assert_eq!(r["tools"][0]["blocked_domains"],json!(["spam.example","ads.example"])); }

#[test]
fn compatible_no_injection() { let m=model("anthropic-messages","https://api.kimi.com/coding",None); let p=json!({"tools":[{"name":"web_search"}]}); assert_eq!(apply(&m,&p,true),p); }

#[test]
fn strips_native_keeps_choice() { let m=model("anthropic-messages","https://api.kimi.com",None); let p=json!({"tool_choice":{"type":"tool","name":"web_search"},"tools":[{"type":"web_search_20250305","name":"web_search"},{"name":"web_search"}]}); let r=apply(&m,&p,true); assert_eq!(r["tools"],json!([{"name":"web_search"}])); assert_eq!(r["tool_choice"],p["tool_choice"]); }

#[test]
fn strips_orphan_choice() { let m=model("anthropic-messages","https://api.kimi.com",None); let r=apply(&m,&json!({"tool_choice":{"name":"web_search"},"tools":[{"type":"web_search_20250305","name":"web_search"}]}),true); assert!(r.get("tools").is_none()); assert!(r.get("tool_choice").is_none()); }

#[test]
fn compat_opt_in() { let m=model("anthropic-messages","https://api.kimi.com",Some(true)); assert_eq!(apply(&m,&json!({}),true)["tools"][0]["name"],"web_search"); }

#[test]
fn disabled() { let m=model("anthropic-messages","https://api.anthropic.com",None); let p=json!({"tools":[{"name":"web_search"}]}); assert_eq!(apply(&m,&p,false),p); }

#[test]
fn default_on() { assert!(parse_enabled(None)); }

#[test]
fn native_support() { assert!(supports_native_anthropic_web_search(Some(&model("anthropic-messages","https://api.anthropic.com",None)))); }

#[test]
fn bare_api_equivalent() { assert!(supports_native_anthropic_web_search(Some(&model("anthropic-messages","https://api.anthropic.com",None)))); }

#[test]
fn compatible_support_false() { assert!(!supports_native_anthropic_web_search(Some(&model("anthropic-messages","https://api.kimi.com",None)))); }

#[test]
fn provider_does_not_override_endpoint() { assert!(!supports_native_anthropic_web_search(Some(&model("anthropic-messages","https://anthropic-proxy.example/v1",None)))); }

#[test]
fn compat_overrides() { assert!(supports_native_anthropic_web_search(Some(&model("anthropic-messages","https://api.kimi.com",Some(true))))); assert!(!supports_native_anthropic_web_search(Some(&model("anthropic-messages","https://api.anthropic.com",Some(false))))); }

#[test]
fn non_anthropic_support_false() { assert!(!supports_native_anthropic_web_search(None)); assert!(!supports_native_anthropic_web_search(Some(&model("openai-responses","https://api.anthropic.com",Some(true))))); }

#[test]
fn env_unset() { assert!(parse_enabled(None)); }

#[test]
fn env_truthy() { for v in ["1","true","yes","on","TRUE","YES","  on  "] { assert!(parse_enabled(Some(v))); } }

#[test]
fn env_falsy() { for v in ["0","false","no","off","OFF","  no  "] { assert!(!parse_enabled(Some(v))); } }

#[test]
fn env_unknown() { for v in ["garbage","enable","enabled"] { assert!(parse_enabled(Some(v))); } }

