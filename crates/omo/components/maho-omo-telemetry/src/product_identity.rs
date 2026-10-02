pub const KNOWN_MODELS: &[(&str, &[&str])] = &[
    ("anthropic", &["claude-fable-5", "claude-haiku-4-5", "claude-opus-5", "claude-sonnet-5"]),
    ("anthropic-api", &["claude-fable-5", "claude-haiku-4-5", "claude-opus-5", "claude-sonnet-5"]),
    ("deepseek", &["deepseek-v4-flash", "deepseek-v4-pro"]),
    ("google", &["gemini-3.6-flash"]),
    ("github-copilot", &["claude-fable-5", "claude-haiku-4-5", "claude-opus-5", "claude-sonnet-5", "gpt-5.6-sol", "gpt-5.6-terra"]),
    ("kimi-for-coding", &["k3", "kimi-for-coding-highspeed", "kimi-k3"]),
    ("moonshotai", &["kimi-k3"]),
    ("openai", &["gpt-5.6-luna-fast", "gpt-5.6-sol", "gpt-5.6-terra"]),
    ("opencode", &["claude-opus-5", "claude-sonnet-5", "gpt-5.6-sol", "kimi-k3"]),
    ("opencode-go", &["deepseek-v4-pro", "kimi-k3", "minimax-m2.7", "minimax-m3"]),
    ("quotio-openai", &["gpt-5.6-luna-fast", "gpt-5.6-sol", "gpt-5.6-terra"]),
    ("vercel", &["claude-fable-5", "claude-haiku-4-5", "claude-opus-5", "claude-sonnet-5", "deepseek-v4-flash", "deepseek-v4-pro", "gemini-3.6-flash", "gpt-5.6-sol", "gpt-5.6-terra", "kimi-k3", "minimax-m2.7", "minimax-m3"]),
    ("xai", &["grok-4.20-0309-non-reasoning"]),
];
use std::{collections::HashMap,io::{Read,Write},os::unix::fs::OpenOptionsExt,path::{Path,PathBuf},sync::{Mutex,OnceLock}};
use sha2::{Digest,Sha256};
pub fn create_omo_native_product_config() -> telemetry_core::TelemetryProductConfig {
    let mut config=crate::index::create_senpi_telemetry_product_config();
    config.cache_dir_name="omo-native".into();
    config.default_api_key="phc_r6UYQzNZcGYSzKw4PxCiVrZepGqV3dw9qcvcKtRNUWAn".into();
    config.event_name="daily_active".into();
    config.product_name="omo-native".into();
    config
}
pub const BUILTIN_SKILL_NAMES: &[&str] = &["ast-grep", "coding-agent-sessions", "dag-library", "data-scientist", "debugging", "frontend", "git-master", "give-me-tips", "hyperplan", "init-deep", "lsp-setup", "mass-ulw", "onboarding", "programming", "refactor", "remove-ai-slops", "review-work", "start-work", "ultimate-browsing", "ultrawork", "ulw-loop", "ulw-plan", "ulw-research", "visual-qa"];
pub const EVENT_PROPERTY_ALLOWLISTS: &[(&str, &[&str])] = &[
    ("daily_active", &["$session_id", "day_utc", "reason"]),
    ("session_started", &["$session_id", "$os", "$os_version", "arch", "cpu_count", "default_model", "default_provider", "memory_bucket", "model_count", "provider_count", "providers", "reason"]),
    ("prompt_submitted", &["$session_id", "input_source", "invocation_stage", "is_effective_ultrawork_invocation", "is_real_user_prompt", "is_turn_start", "keyword_any", "keyword_occurrence_bucket", "keyword_ultrawork_full", "keyword_ulw_abbrev", "keyword_variant", "prompt_length_bucket", "queue_mode", "real_prompt_ordinal_bucket", "suppression_reason"]),
    ("turn_completed", &["$session_id", "cache_read_tokens", "cache_write_tokens", "cost_usd", "input_tokens", "model_id", "output_tokens", "provider", "reasoning_tokens", "total_tokens", "turn_index"]),
    ("skill_loaded", &["$session_id", "skill_name"]),
    ("delegation_started", &["$session_id", "background", "batch_size_bucket", "kind", "name"]),
    ("feature_used", &["$session_id", "feature"]),
];
pub fn builtin_category_names()->Vec<&'static str> {senpi_task::category::BUILTIN_CATEGORY_DEFAULTS.iter().map(|c|c.name).collect()}
pub fn curated_agents()->std::collections::BTreeSet<&'static str> {senpi_task::agents::curated_readonly_agent_names()}
pub fn omo_native_event_schemas()->serde_json::Value {
    use serde_json::{Value,json};
    let mut events=serde_json::Map::new();
    for &(event,keys) in EVENT_PROPERTY_ALLOWLISTS {
        let mut properties=serde_json::Map::new();
        for &key in keys {
            let values:Option<Vec<&str>>=match (event,key) {
                (_,"default_provider"|"provider")=>Some(KNOWN_MODELS.iter().map(|(p,_)|*p).chain(["custom"]).collect()),
                (_,"default_model"|"model_id")=>{let mut models=Vec::new();for (_,names) in KNOWN_MODELS {for &name in *names {if !models.contains(&name) {models.push(name);}}}models.push("custom");Some(models)},
                ("daily_active","reason")=>Some(vec!["session_start"]),
                ("session_started","reason")=>Some(vec!["startup","reload","new","resume","fork"]),
                (_,"memory_bucket")=>Some(vec!["lt_8_gb","8_15_gb","16_31_gb","32_63_gb","64_plus_gb"]),
                (_,"input_source")=>Some(vec!["interactive","rpc","extension"]),
                (_,"invocation_stage")=>Some(vec!["none","first_arm","remention","post_compact_rearm"]),
                (_,"keyword_occurrence_bucket")=>Some(vec!["1","2","3_5","6_plus"]),
                (_,"keyword_variant")=>Some(vec!["none","ulw","ultrawork","both"]),
                (_,"prompt_length_bucket")=>Some(vec!["lt_100","100_500","500_2000","gte_2000"]),
                (_,"queue_mode")=>Some(vec!["immediate","follow_up","steer","other"]),
                (_,"real_prompt_ordinal_bucket")=>Some(vec!["1","2_3","4_10","11_25","26_plus"]),
                (_,"suppression_reason")=>Some(vec!["none","no_keyword","extension_source","embedded_directive","skill_expansion","skill_name_only"]),
                (_,"skill_name")=>Some(BUILTIN_SKILL_NAMES.to_vec()),
                (_,"batch_size_bucket")=>Some(vec!["1","2_4","5_plus"]),
                (_,"kind")=>Some(vec!["category","subagent"]),
                (_,"name")=>Some(builtin_category_names().into_iter().chain(curated_agents()).chain(["custom"]).collect()),
                (_,"feature")=>Some(vec!["goal_tool","team_create","memory_tool"]),
                _=>None,
            };
            let property=if let Some(values)=values {json!({"type":"string","values":values})} else {let kind=if key.starts_with("is_") || matches!(key,"keyword_any"|"keyword_ultrawork_full"|"keyword_ulw_abbrev"|"background") {"boolean"} else if key.ends_with("_count") || key.ends_with("_tokens") || matches!(key,"cost_usd"|"turn_index") {"number"} else {"string"};json!({"type":kind})};
            properties.insert(key.into(),property);
        }
        events.insert(event.into(),Value::Object(properties));
    }
    events.insert("parallelism_summary".into(),Value::Object(crate::parallelism_schema::PARALLELISM_SUMMARY_SCHEMA.iter().map(|(key,kind)|((*key).into(),if *key=="schema_kind" {json!({"type":"string","values":["parallelism_v1","parallelism_v2"]})} else {json!({"type":kind})})).collect()));
    Value::Object(events)
}
static FALLBACK_SALTS:OnceLock<Mutex<HashMap<PathBuf,[u8;32]>>>=OnceLock::new();
pub fn get_omo_native_state_dir(env:&telemetry_core::TelemetryEnv)->PathBuf {
    let legacy=crate::index::get_senpi_telemetry_state_dir(env);
    legacy.parent().unwrap_or(&legacy).join("omo-native")
}
pub fn hash_session_id(raw:&str,state_dir:&Path)->std::io::Result<String> {
    let path=state_dir.join("session-id-salt");
    let read=||std::fs::read(&path).ok().and_then(|b|<[u8;32]>::try_from(b).ok());
    let salt=if let Some(salt)=read() {salt} else {
        let mut salt=[0;32];std::fs::File::open("/dev/urandom")?.read_exact(&mut salt)?;
        let write=|exclusive:bool|->std::io::Result<()> {std::fs::create_dir_all(state_dir)?;let mut file=std::fs::OpenOptions::new().write(true).create(!exclusive).create_new(exclusive).truncate(!exclusive).mode(0o600).open(&path)?;file.write_all(&salt)};
        if write(true).is_ok() {FALLBACK_SALTS.get_or_init(Default::default).lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&path);salt}
        else if let Some(existing)=read() {existing}
        else if write(false).is_ok() {FALLBACK_SALTS.get_or_init(Default::default).lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&path);salt}
        else {*FALLBACK_SALTS.get_or_init(Default::default).lock().unwrap_or_else(std::sync::PoisonError::into_inner).entry(path).or_insert(salt)}
    };
    let mut hash=Sha256::new();hash.update(salt);hash.update(raw.as_bytes());Ok(format!("{:x}",hash.finalize()))
}
pub fn mask_provider_and_model(provider: &str, model: &str) -> (String, String) {
    let models=KNOWN_MODELS.iter().find(|(p,_)| *p == provider).map(|(_,m)| *m);
    (if models.is_some() {provider} else {"custom"}.into(), if models.is_some_and(|m|m.contains(&model)) {model} else {"custom"}.into())
}
#[cfg(test)]
mod identity_tests {
    use super::*;
    #[test] fn native_product_identity() {let c=create_omo_native_product_config();assert_eq!(c.platform,"omo-senpi");assert_eq!(c.machine_id_prefix,"omo-senpi:");assert_eq!(c.product_env_prefix,"OMO_SENPI");assert_eq!(c.event_name,"daily_active");assert_eq!(c.cache_dir_name,"omo-native");assert!(!c.default_api_key.is_empty());}
    #[test] fn explicit_agent_state_dir() {let e=telemetry_core::TelemetryEnv::from([("SENPI_CODING_AGENT_DIR".into(),"/fixture/agent".into())]);assert_eq!(get_omo_native_state_dir(&e),PathBuf::from("/fixture/agent/omo-senpi/omo-native"));}
    #[test] fn deleted_salt_recreated() {let t=tempfile::tempdir().unwrap();hash_session_id("s",t.path()).unwrap();std::fs::remove_file(t.path().join("session-id-salt")).unwrap();hash_session_id("s",t.path()).unwrap();assert!(t.path().join("session-id-salt").is_file());}
    #[test] fn histogram_privacy_bound() {let widest=std::iter::repeat_n(crate::wave_assembler::MAX_TRACKED_CALLS.to_string(),8).collect::<Vec<_>>().join(":");assert_eq!(widest.len(),39);assert!(widest.len()<=64);}
    #[test] fn static_allowlists_unique() {assert!(!BUILTIN_SKILL_NAMES.is_empty());for (_,keys) in EVENT_PROPERTY_ALLOWLISTS {let unique:std::collections::HashSet<_>=keys.iter().collect();assert_eq!(unique.len(),keys.len());}}
    #[test] fn imported_task_names_exact() {assert_eq!(builtin_category_names(),senpi_task::category::BUILTIN_CATEGORY_DEFAULTS.iter().map(|c|c.name).collect::<Vec<_>>());assert_eq!(curated_agents(),senpi_task::agents::curated_readonly_agent_names());}
    #[test] fn schema_keys_match_capture_allowlists() {let schemas=omo_native_event_schemas();for (name,keys) in EVENT_PROPERTY_ALLOWLISTS {let mut actual=schemas[*name].as_object().unwrap().keys().map(String::as_str).collect::<Vec<_>>();let mut expected=keys.to_vec();actual.sort();expected.sort();assert_eq!(actual,expected);}assert_eq!(schemas["parallelism_summary"].as_object().unwrap().len(),crate::parallelism_schema::PARALLELISM_SUMMARY_SCHEMA.len());}
    #[test] fn schemas_preserve_closed_vocabularies_and_types() {let s=omo_native_event_schemas();assert_eq!(s["session_started"]["cpu_count"]["type"],"number");assert_eq!(s["prompt_submitted"]["keyword_any"]["type"],"boolean");assert_eq!(s["feature_used"]["feature"]["values"],serde_json::json!(["goal_tool","team_create","memory_tool"]));assert_eq!(s["parallelism_summary"]["schema_kind"]["values"],serde_json::json!(["parallelism_v1","parallelism_v2"]));assert!(s["delegation_started"]["name"]["values"].as_array().unwrap().contains(&serde_json::json!(builtin_category_names()[0])));assert!(!s.to_string().contains("prompt_text"));}
    #[test] fn salted_hash_stable_private() {let t=tempfile::tempdir().unwrap();let a=hash_session_id("private-session",t.path()).unwrap();assert_eq!(a,hash_session_id("private-session",t.path()).unwrap());assert_ne!(a,hash_session_id("other-session",t.path()).unwrap());assert_eq!(a.len(),64);assert!(!a.contains("private-session"));assert_eq!(std::fs::read(t.path().join("session-id-salt")).unwrap().len(),32);}
    #[test] fn salt_file_private() {use std::os::unix::fs::PermissionsExt;let t=tempfile::tempdir().unwrap();hash_session_id("s",t.path()).unwrap();assert_eq!(std::fs::metadata(t.path().join("session-id-salt")).unwrap().permissions().mode()&0o777,0o600);}
    #[test] fn invalid_salt_repaired() {let t=tempfile::tempdir().unwrap();std::fs::write(t.path().join("session-id-salt"),"invalid").unwrap();hash_session_id("s",t.path()).unwrap();assert_eq!(std::fs::read(t.path().join("session-id-salt")).unwrap().len(),32);}
    #[test] fn fallback_salt_stable() {let t=tempfile::tempdir().unwrap();let blocked=t.path().join("file");std::fs::write(&blocked,"").unwrap();assert_eq!(hash_session_id("s",&blocked).unwrap(),hash_session_id("s",&blocked).unwrap());}
}
