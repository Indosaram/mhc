use maho_ai::types::Message;
use maho_ext_compaction::lane_policy::*;
use serde_json::{Value,json};
fn load(_: &str)->Result<Option<String>,()> {Ok(Some("auto".into()))}
fn boundary()->Value {json!({"type":"system","subtype":"compact_boundary","uuid":"u","session_id":"s","compact_metadata":{"trigger":"auto","pre_tokens":120000}})}
fn assistant(diagnostics:Value)->Message {serde_json::from_value(json!({"role":"assistant","content":[],"api":"claude-sdk-oauth","provider":"anthropic-subscription","model":"model","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":"stop","timestamp":1,"diagnostics":diagnostics})).expect("fixture")}
#[test] fn native_when_resume_auto() {assert!(is_sdk_native_compaction_lane(Some("anthropic-subscription"),Some("auto")));}
#[test] fn native_when_resume_unset() {assert!(is_sdk_native_compaction_lane(Some("anthropic-subscription"),None));}
#[test] fn senpi_owned_when_resume_off() {assert!(!is_sdk_native_compaction_lane(Some("anthropic-subscription"),Some("off")));}
#[test] fn other_providers_are_not_claimed() {for provider in ["anthropic","openai"] {assert!(!is_sdk_native_compaction_lane(Some(provider),Some("auto")));}}
#[test] fn unknown_model_is_not_claimed() {assert!(!is_sdk_native_compaction_lane(None,None));}
#[test] fn settings_are_cached_when_cwd_matches() {let mut policy=CompactionLanePolicy::default();let mut loads=0;for _ in 0..2 {assert!(policy.disables_senpi_compaction("/repo",Some("anthropic-subscription"),None,|_|{loads+=1;Ok::<_,()>(Some("auto".into()))}));}assert_eq!(loads,1);}
#[test] fn settings_not_loaded_when_provider_is_foreign() {let mut policy=CompactionLanePolicy::default();assert!(!policy.disables_senpi_compaction("/repo",Some("anthropic"),None,|_|->Result<Option<String>,()>{panic!("must not load")}));}
#[test] fn settings_reload_when_cwd_changes() {let mut policy=CompactionLanePolicy::default();let mut seen=Vec::new();for (cwd,expected) in [("/auto",true),("/off",false)] {assert_eq!(policy.disables_senpi_compaction(cwd,Some("anthropic-subscription"),None,|cwd|{seen.push(cwd.to_owned());Ok::<_,()>(Some(if cwd=="/off" {"off"} else {"auto"}.into()))}),expected);}assert_eq!(seen,["/auto","/off"]);}
#[test] fn settings_failure_keeps_senpi_active() {let mut policy=CompactionLanePolicy::default();assert!(!policy.disables_senpi_compaction("/repo",Some("anthropic-subscription"),None,|_|Err::<Option<String>,_>(() )));}
#[test] fn override_reenables_senpi() {assert!(!CompactionLanePolicy::default().disables_senpi_compaction("/repo",Some("anthropic-subscription"),Some("deepseek/chat"),load));}
#[test] fn empty_override_stands_down() {assert!(CompactionLanePolicy::default().disables_senpi_compaction("/repo",Some("anthropic-subscription"),Some(""),load));}
#[test] fn boundary_parses_when_fields_are_valid() {let entry=parse_compact_boundary_message(&boundary()).expect("boundary");assert_eq!(entry.sdk_session_id,"s");assert_eq!(entry.uuid,"u");assert_eq!(entry.compact_metadata["pre_tokens"],120000);}
#[test] fn boundary_rejects_invalid_messages() {for value in [Value::Null,json!({"type":"system","subtype":"init","uuid":"u","session_id":"s"}),json!({"type":"system","subtype":"compact_boundary"})] {assert!(parse_compact_boundary_message(&value).is_none());}}
#[test] fn diagnostic_boundaries_are_collected() {let message=assistant(json!([{"type":ANTHROPIC_SUBSCRIPTION_COMPACT_BOUNDARY_DIAGNOSTIC,"timestamp":5,"details":boundary()}]));assert_eq!(collect_compact_boundary_entries(&message),vec![parse_compact_boundary_message(&boundary()).expect("boundary")]);}
#[test] fn unrelated_diagnostics_are_ignored() {for diagnostics in [Value::Null,json!([{"type":"other","timestamp":1,"details":{"kind":"delta"}}])] {assert!(collect_compact_boundary_entries(&assistant(diagnostics)).is_empty());}}
#[test] fn ledger_entry_type_is_stable() {assert_eq!(ANTHROPIC_SUBSCRIPTION_COMPACT_ENTRY_TYPE,"claude-sdk-oauth-compact");}
#[test] fn manual_recovery_is_owned_even_when_sdk_native() {assert!(CompactionLanePolicy::default().owns_compaction("/repo",Some("anthropic-subscription"),None,"manual",|_|->Result<Option<String>,()>{panic!("manual does not load")}));}
