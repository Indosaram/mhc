use maho_ext_builtin_loose::service_tier::*;
use maho_ext_api::ServiceTier;
use serde_json::json;
#[test]
fn supported_payload_injects_only_absent_tier(){assert_eq!(add_service_tier_to_payload(Some("openai-responses"),json!({"model":"m"}),Some(ServiceTier::Priority)),json!({"model":"m","service_tier":"priority"}));for payload in [json!({"service_tier":null}),json!({"service_tier":"flex"}),json!([]),json!(null)]{assert_eq!(add_service_tier_to_payload(Some(CODEX_RESPONSES_API),payload.clone(),Some(ServiceTier::Priority)),payload);}}
#[test]
fn unsupported_api_and_absent_setting_leave_payload(){for api in [None,Some("anthropic-messages"),Some("openai-completions")]{assert!(!supports_service_tier(api));assert_eq!(add_service_tier_to_payload(api,json!({}),Some(ServiceTier::Priority)),json!({}));}assert_eq!(add_service_tier_to_payload(Some(CODEX_RESPONSES_API),json!({}),None),json!({}));}
#[test]
fn remembered_auto_suppresses_only_catalog_priority_on_same_model(){for (same,catalog,expected) in [(true,true,None),(false,true,Some(ServiceTier::Priority)),(true,false,Some(ServiceTier::Priority))]{assert_eq!(effective_service_tier(Some(CODEX_RESPONSES_API),false,Some(ServiceTier::Priority),None,Some(ServiceTier::Auto),same,catalog),expected);}assert_eq!(effective_service_tier(Some(CODEX_RESPONSES_API),true,None,None,Some(ServiceTier::Auto),true,true),Some(ServiceTier::Priority));assert_eq!(effective_service_tier(Some("openai-responses"),false,None,Some(ServiceTier::Flex),None,false,false),Some(ServiceTier::Flex));}
