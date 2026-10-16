use serde_json::{Value,Map};
use std::collections::BTreeMap;
use std::sync::{Mutex,OnceLock};
use sha2::Digest;
pub struct ProviderSettings {pub values:Map<String,Value>,pub prompt_mode_from_env:bool}
fn valid_choice(key:&str,value:&Value)->bool {let choices:&[&str]=match key {"systemPromptMode"=>&["preset-append","full","override"],"resumeMode"=>&["auto","off"],"tokenInjection"=>&["oauth-slots","config-dir","ambient"],_=>return false};value.as_str().is_some_and(|s|choices.contains(&s))}
fn parse(value:&Value)->Map<String,Value> {
    let mut result=Map::new();let Some(object)=value.as_object() else {return result;};
    for (key,value) in object {let valid=match key.as_str() {
        "enabled"|"appendSystemPrompt"|"strictMcpConfig"=>value.is_boolean(),
        "systemPromptFile"|"pinnedAccount"=>value.as_str().is_some_and(|s|!s.is_empty()),
        "settingSources"=>value.as_array().is_some_and(|items|items.iter().all(|v|v.as_str().is_some_and(|s|["user","project","local"].contains(&s)))),
        "compactionOwner"=>value.as_str().is_some_and(|s|["senpi","sdk"].contains(&s)),
        _=>valid_choice(key,value),
    };if valid {result.insert(key.clone(),value.clone());}}result
}
pub fn load(global:&Value,project:&Value,environment:&BTreeMap<String,String>)->ProviderSettings {
    let block=|settings:&Value|settings.get("anthropicSubscriptionProvider").filter(|v|!v.is_null()).or_else(||settings.get("claudeSdkOauthProvider")).cloned().unwrap_or(Value::Null);
    let mut values=parse(&block(global));values.extend(parse(&block(project)));let mut env=Map::new();
    for (suffix,key) in [("SYSTEM_PROMPT_MODE","systemPromptMode"),("SYSTEM_PROMPT_FILE","systemPromptFile"),("RESUME","resumeMode"),("TOKEN_INJECTION","tokenInjection"),("PINNED_ACCOUNT","pinnedAccount")] {if let Some(value)=environment.get(&format!("SENPI_CLAUDE_SDK_OAUTH_{suffix}")) {env.insert(key.into(),Value::String(value.clone()));}}
    if let Some(value)=environment.get("SENPI_CLAUDE_SDK_OAUTH_ENABLED") {match value.to_lowercase().as_str() {"true"|"1"=>{env.insert("enabled".into(),Value::Bool(true));},"false"|"0"=>{env.insert("enabled".into(),Value::Bool(false));},_=>{}}}
    if let Some(value)=environment.get("SENPI_CLAUDE_SDK_OAUTH_SETTING_SOURCES") {env.insert("settingSources".into(),Value::Array(if value.is_empty() {Vec::new()}else {value.split(',').map(|s|Value::String(s.trim().into())).collect()}));}
    let env=parse(&Value::Object(env));let prompt_mode_from_env=env.contains_key("systemPromptMode");values.extend(env);ProviderSettings {values,prompt_mode_from_env}
}
#[derive(Debug,PartialEq,Eq)]
pub struct ResolvedPromptMode {pub mode:String,pub source:&'static str,pub conflict:bool}
pub fn resolve_prompt_mode(settings:&ProviderSettings)->ResolvedPromptMode {
    if let Some(mode)=settings.values.get("systemPromptMode").and_then(Value::as_str) {return ResolvedPromptMode {mode:mode.into(),source:if settings.prompt_mode_from_env {"env"}else {"setting"},conflict:settings.values.contains_key("appendSystemPrompt")};}
    if let Some(append)=settings.values.get("appendSystemPrompt").and_then(Value::as_bool) {return ResolvedPromptMode {mode:if append {"full"}else {"preset-append"}.into(),source:"legacy",conflict:false};}
    ResolvedPromptMode {mode:"full".into(),source:"default",conflict:false}
}
pub fn resolve_compaction_owner(settings:&ProviderSettings)->&str {
    settings.values.get("compactionOwner").and_then(Value::as_str).unwrap_or("senpi")
}
struct DiskCache { key:String, global:Value, project:Value }
fn disk_cache()->&'static Mutex<Option<DiskCache>> { static CACHE:OnceLock<Mutex<Option<DiskCache>>>=OnceLock::new();CACHE.get_or_init(||Mutex::new(None)) }
pub fn load_from_disk(cwd:&std::path::Path,agent_dir:&std::path::Path,home_dir:&std::path::Path,environment:&BTreeMap<String,String>)->ProviderSettings {
    let global_path=maho_core::settings_manager::get_settings_path(&cwd.to_string_lossy(),&agent_dir.to_string_lossy(),maho_core::settings_manager::SettingsScope::Global,&home_dir.to_string_lossy());
    let project_path=maho_core::settings_manager::get_settings_path(&cwd.to_string_lossy(),&agent_dir.to_string_lossy(),maho_core::settings_manager::SettingsScope::Project,&home_dir.to_string_lossy());
    let revision=|path:&str|std::fs::read(path).map(|bytes|format!("{:x}",sha2::Sha256::digest(bytes))).unwrap_or_else(|_|"missing".into());
    let key=format!("{}|{}|{}|{}|{}",cwd.display(),global_path,revision(&global_path),project_path,revision(&project_path));
    let mut cache=disk_cache().lock().expect("settings disk cache");
    if cache.as_ref().is_none_or(|cached|cached.key!=key) {
        let load=|path:&str|std::fs::read_to_string(path).ok().and_then(|text|maho_core::settings_manager::parse_settings_json(&text).ok()).map(Value::Object).unwrap_or(Value::Null);
        *cache=Some(DiskCache {key,global:load(&global_path),project:load(&project_path)});
    }
    let cached=cache.as_ref().expect("settings cache populated");load(&cached.global,&cached.project,environment)
}
#[cfg(test)]
mod tests {
    use super::*;use serde_json::json;
    #[test]
    fn settings_precedence_and_invalid_values() {
        let global=json!({"claudeSdkOauthProvider":{"enabled":true,"resumeMode":"auto","pinnedAccount":"global"}});let project=json!({"anthropicSubscriptionProvider":{"enabled":false,"resumeMode":"bad","pinnedAccount":"project","settingSources":["user"]}});
        let environment=[("SENPI_CLAUDE_SDK_OAUTH_ENABLED".into(),"TRUE".into()),("SENPI_CLAUDE_SDK_OAUTH_SETTING_SOURCES".into(),"".into())].into();let settings=load(&global,&project,&environment);
        assert_eq!(settings.values["enabled"],true);assert_eq!(settings.values["resumeMode"],"auto");assert_eq!(settings.values["pinnedAccount"],"project");assert_eq!(settings.values["settingSources"],json!([]));
    }
    #[test]
    fn prompt_modes_preserve_source_and_legacy_conflict() {
        let settings=load(&json!({}),&json!({}),&BTreeMap::new());assert_eq!(resolve_prompt_mode(&settings).source,"default");
        let project=json!({"anthropicSubscriptionProvider":{"appendSystemPrompt":false}});let settings=load(&json!({}),&project,&BTreeMap::new());assert_eq!(resolve_prompt_mode(&settings).mode,"preset-append");
        let environment=[("SENPI_CLAUDE_SDK_OAUTH_SYSTEM_PROMPT_MODE".into(),"override".into())].into();let settings=load(&json!({}),&project,&environment);assert_eq!(resolve_prompt_mode(&settings),ResolvedPromptMode {mode:"override".into(),source:"env",conflict:true});
    }
    #[test]
    fn compaction_owner_defaults_and_obeys_setting_precedence() {
        let settings=load(&json!({}),&json!({}),&BTreeMap::new());assert_eq!(resolve_compaction_owner(&settings),"senpi");
        let settings=load(&json!({"anthropicSubscriptionProvider":{"compactionOwner":"sdk"}}),&json!({"anthropicSubscriptionProvider":{"compactionOwner":"senpi"}}),&BTreeMap::new());assert_eq!(resolve_compaction_owner(&settings),"senpi");
    }
    #[test]
    fn disk_settings_cache_reuses_revision_and_invalidates_on_content_change() {
        let directory=tempfile::tempdir().expect("directory");let agent=directory.path().join("agent");std::fs::create_dir_all(&agent).expect("agent dir");
        let project=directory.path().join(".omo");std::fs::create_dir_all(&project).expect("project dir");let settings_file=project.join("settings.json");
        std::fs::write(&settings_file,r#"{"anthropicSubscriptionProvider":{"enabled":true}}"#).expect("settings");
        let env=BTreeMap::new();let first=load_from_disk(directory.path(),&agent,directory.path(),&env);assert_eq!(first.values["enabled"],true);
        std::fs::write(&settings_file,r#"{"anthropicSubscriptionProvider":{"enabled":false}}"#).expect("update settings");
        let second=load_from_disk(directory.path(),&agent,directory.path(),&env);assert_eq!(second.values["enabled"],false);
    }
}
