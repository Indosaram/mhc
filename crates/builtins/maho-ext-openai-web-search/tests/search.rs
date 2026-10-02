use maho_ext_openai_web_search::*;
use maho_ext_api::Model;
use serde_json::{json,Value};
fn model(api:&str,url:&str,override_value:Option<bool>)->Model {
 let mut value=json!({"id":"test","name":"Test","api":api,"provider":"openai","baseUrl":url,"reasoning":false,"input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":1000,"maxTokens":100});
 if let Some(v)=override_value { value["compat"]=json!({"supportsWebSearchPreview":v}); }
 serde_json::from_value(value).expect("valid model fixture")
}
fn apply(api:&str,p:&Value,enabled:bool)->Value { add_openai_web_search_to_payload(Some(&model(api,"https://api.openai.com/v1",None)),p,enabled) }
#[test]
fn chat_noop() { let p=json!({"tools":[{"name":"web_search"}]}); assert_eq!(apply("openai-completions",&p,true),p); }

#[test]
fn anthropic_noop() { let p=json!({"tools":[{"name":"web_search"}]}); assert_eq!(apply("anthropic-messages",&p,true),p); }

#[test]
fn strip_anthropic() { assert_eq!(apply("anthropic-messages",&json!({"tools":[{"name":"other"},{"type":"web_search_preview"}]}),true)["tools"],json!([{"name":"other"}])); }

#[test]
fn strip_dated() { assert_eq!(apply("anthropic-messages",&json!({"tools":[{"type":"web_search_preview_2025_03_11"},{"name":"keeper"}]}),true)["tools"],json!([{"name":"keeper"}])); }

#[test]
fn strip_chat() { assert_eq!(apply("openai-completions",&json!({"tools":[{"type":"web_search_preview"},{"name":"keeper"}]}),true)["tools"],json!([{"name":"keeper"}])); }

#[test]
fn strip_disabled() { assert_eq!(apply("anthropic-messages",&json!({"tools":[{"type":"web_search_preview"}]}),false)["tools"],json!([])); }

#[test]
fn keep_anthropic_native() { let p=json!({"tools":[{"type":"web_search_20250305"},{"type":"web_fetch_20260309"}]}); assert_eq!(apply("anthropic-messages",&p,true),p); }

#[test]
fn inject() { assert_eq!(apply("openai-responses",&json!({}),true)["tools"],json!([{"type":"web_search_preview"}])); }

#[test]
fn include_sources() { assert_eq!(apply("openai-responses",&json!({}),true)["include"],json!(["web_search_call.action.sources"])); }

#[test]
fn include_once() { let p=json!({"include":["reasoning.encrypted_content","web_search_call.action.sources"],"tools":[{"type":"web_search_preview"}]}); assert_eq!(apply("openai-responses",&p,true)["include"],p["include"]); }

#[test]
fn azure_inject() { assert_eq!(apply("azure-openai-responses",&json!({}),true)["tools"][0]["type"],"web_search_preview"); }

#[test]
fn native_once() { let p=json!({"tools":[{"type":"web_search_preview"},{"name":"other"}]}); assert_eq!(apply("openai-responses",&p,true)["tools"],p["tools"]); }

#[test]
fn replace_function() { assert_eq!(apply("openai-responses",&json!({"tools":[{"name":"web_search"}]}),true)["tools"],json!([{"type":"web_search_preview"}])); }

#[test]
fn strip_foreign_native() { assert_eq!(apply("openai-responses",&json!({"tools":[{"type":"function","name":"other"},{"type":"web_search_20250305"},{"type":"web_fetch_20260309"}]}),true)["tools"],json!([{"type":"function","name":"other"},{"type":"web_search_preview"}])); }

#[test]
fn keep_webfetch() { assert_eq!(apply("openai-responses",&json!({"tools":[{"name":"webfetch"},{"type":"web_fetch_20260309"}]}),true)["tools"][0],json!({"name":"webfetch"})); }

#[test]
fn disabled_strips_foreign() { assert_eq!(apply("openai-responses",&json!({"tools":[{"name":"other"},{"type":"web_fetch_20260309"}]}),false)["tools"],json!([{"name":"other"}])); }

#[test]
fn nonresponses_keeps_function() { let p=json!({"tools":[{"name":"web_search"}]}); assert_eq!(apply("openai-completions",&p,true),p); }

#[test]
fn disabled_keeps_function() { let p=json!({"tools":[{"name":"web_search"}]}); assert_eq!(apply("openai-responses",&p,false),p); }

#[test]
fn default_enabled() { assert!(parse_enabled(None)); }

#[test]
fn proxy_strip() { let m=model("openai-responses","https://quotio.example/v1",None); let p=json!({"tools":[{"type":"web_search_preview"},{"name":"web_search"}],"include":["reasoning.encrypted_content","web_search_call.action.sources"],"tool_choice":{"type":"web_search_preview"}}); let r=add_openai_web_search_to_payload(Some(&m),&p,true); assert_eq!(r["tools"],json!([{"name":"web_search"}])); assert_eq!(r["include"],json!(["reasoning.encrypted_content"])); assert!(r.get("tool_choice").is_none()); }

#[test]
fn proxy_optin() { let m=model("openai-responses","https://quotio.example/v1",Some(true)); assert_eq!(add_openai_web_search_to_payload(Some(&m),&json!({}),true)["tools"][0]["type"],"web_search_preview"); }

#[test]
fn unset_true() { assert!(parse_enabled(None)); }

#[test]
fn truthy() { for v in ["1","true","yes","on","TRUE","YES","  on  "] { assert!(parse_enabled(Some(v))); } }

#[test]
fn falsy() { for v in ["0","false","no","off","OFF","  no  "] { assert!(!parse_enabled(Some(v))); } }

#[test]
fn unknown() { for v in ["garbage","enable","enabled"] { assert!(parse_enabled(Some(v))); } }

