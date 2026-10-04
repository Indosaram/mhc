use std::collections::{BTreeMap, BTreeSet};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutionMode { Agent, Plan }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResumeMode { Auto, Off }
#[derive(Clone, Debug, PartialEq)]
pub struct CursorCliOauthProviderSettings {
    pub enabled: bool,
    pub explicitly_disabled: bool,
    pub executable_path: Option<String>,
    pub force_execution: bool,
    pub no_approval_acknowledged_at: Option<String>,
    pub execution_mode: ExecutionMode,
    pub resume_mode: ResumeMode,
    pub pinned_account: Option<String>,
    pub context_recap_on_model_switch: bool,
    pub model_catalog_ttl_hours: f64,
    pub sandbox_mode: Option<String>,
    pub deny_commands: Vec<String>,
}
impl Default for CursorCliOauthProviderSettings {
    fn default() -> Self {
        Self { enabled: false, explicitly_disabled: false, executable_path: None, force_execution: true,
            no_approval_acknowledged_at: None, execution_mode: ExecutionMode::Agent, resume_mode: ResumeMode::Auto,
            pinned_account: None, context_recap_on_model_switch: true, model_catalog_ttl_hours:24.0,
            sandbox_mode:None, deny_commands: Vec::new() }
    }
}
fn nonempty(value: Option<&str>) -> Option<String> { value.filter(|v| !v.is_empty()).map(str::to_owned) }
fn boolean(value: &str) -> Option<bool> {
    match value.to_lowercase().as_str() { "1" | "true" => Some(true), "0" | "false" => Some(false), _ => None }
}
fn positive(value: f64) -> Option<f64> { (value.is_finite() && value > 0.0).then_some(value) }
fn iso(value: &str) -> bool {
    regex::Regex::new(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d{3})?Z$").is_ok_and(|r| r.is_match(value))
        && chrono::DateTime::parse_from_rfc3339(value).is_ok()
}
fn apply_disk(settings: &mut CursorCliOauthProviderSettings, value: &Value) {
    if let Some(enabled) = value["enabled"].as_bool() { settings.enabled = enabled; settings.explicitly_disabled = !enabled; }
    if let Some(path) = nonempty(value["executablePath"].as_str()) { settings.executable_path = Some(path); }
    if let Some(force) = value["forceExecution"].as_bool() { settings.force_execution = force; }
    if let Some(at) = value["noApprovalAcknowledgedAt"].as_str().filter(|v| iso(v)) { settings.no_approval_acknowledged_at = Some(at.into()); }
    match value["executionMode"].as_str() { Some("agent") => settings.execution_mode = ExecutionMode::Agent,
        Some("plan") => settings.execution_mode = ExecutionMode::Plan, _ => {} }
    match value["resumeMode"].as_str() { Some("auto") => settings.resume_mode = ResumeMode::Auto,
        Some("off") => settings.resume_mode = ResumeMode::Off, _ => {} }
    if let Some(pin) = nonempty(value["pinnedAccount"].as_str()) { settings.pinned_account = Some(pin); }
    if let Some(recap) = value["contextRecapOnModelSwitch"].as_bool() { settings.context_recap_on_model_switch = recap; }
    if let Some(ttl) = value["modelCatalogTtlHours"].as_f64().and_then(positive) { settings.model_catalog_ttl_hours = ttl; }
    if let Some(mode) = nonempty(value["sandboxMode"].as_str()) { settings.sandbox_mode = Some(mode); }
    if let Some(commands) = value["denyCommands"].as_array() {
        settings.deny_commands = commands.iter().filter_map(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned).collect();
    }
}
pub fn resolve_settings(layers: &[Value], environment: &BTreeMap<String, String>) -> CursorCliOauthProviderSettings {
    let mut settings = CursorCliOauthProviderSettings::default();
    for layer in layers { apply_disk(&mut settings, layer); }
    let env = |key: &str| environment.get(key).map(String::as_str);
    if let Some(path) = nonempty(env("SENPI_CURSOR_CLI_OAUTH_EXECUTABLE")).or_else(|| nonempty(env("CURSOR_AGENT_EXECUTABLE"))) { settings.executable_path = Some(path); }
    if let Some(enabled) = env("SENPI_CURSOR_CLI_OAUTH_ENABLED").and_then(boolean) { settings.enabled = enabled; settings.explicitly_disabled = !enabled; }
    if let Some(force) = env("SENPI_CURSOR_CLI_OAUTH_FORCE").and_then(boolean) { settings.force_execution = force; }
    match env("SENPI_CURSOR_CLI_OAUTH_EXECUTION_MODE") { Some("agent") => settings.execution_mode = ExecutionMode::Agent,
        Some("plan") => settings.execution_mode = ExecutionMode::Plan, _ => {} }
    match env("SENPI_CURSOR_CLI_OAUTH_RESUME") { Some("auto") => settings.resume_mode = ResumeMode::Auto,
        Some("off") => settings.resume_mode = ResumeMode::Off, _ => {} }
    if let Some(pin) = nonempty(env("SENPI_CURSOR_CLI_OAUTH_PINNED_ACCOUNT")) { settings.pinned_account = Some(pin); }
    if let Some(recap) = env("SENPI_CURSOR_CLI_OAUTH_RECAP").and_then(boolean) { settings.context_recap_on_model_switch = recap; }
    if let Some(ttl) = env("SENPI_CURSOR_CLI_OAUTH_MODEL_CATALOG_TTL_HOURS").and_then(|v| v.trim().parse().ok()).and_then(positive) { settings.model_catalog_ttl_hours = ttl; }
    if let Some(mode) = nonempty(env("SENPI_CURSOR_CLI_OAUTH_SANDBOX_MODE")) { settings.sandbox_mode = Some(mode); }
    if let Some(commands) = env("SENPI_CURSOR_CLI_OAUTH_DENY_COMMANDS").filter(|v| !v.is_empty()) {
        settings.deny_commands = commands.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned).collect();
    }
    settings
}
pub fn parse_cursor_cli_oauth_provider_settings(value: &Value, environment: &BTreeMap<String, String>) -> CursorCliOauthProviderSettings {
    resolve_settings(std::slice::from_ref(value), environment)
}
pub fn create_sandbox_mode_validator(accepted: BTreeSet<String>, mut on_warning: impl FnMut(String)) -> impl FnMut(Option<&str>) -> Option<String> {
    let mut warned = BTreeSet::new();
    move |value| {
        let value = value?;
        if accepted.contains(value) { return Some(value.into()); }
        if warned.insert(value.to_owned()) { on_warning(format!("Ignoring unrecognized Cursor CLI OAuth sandbox mode: {value}")); }
        None
    }
}

pub fn persist_provider_patch(storage: &dyn maho_core::settings_manager::SettingsStorage, action: &str, patch: &serde_json::Map<String,Value>) -> Result<(),String> {
    use maho_core::settings_manager::{SettingsScope,parse_settings_json};
    storage.select_source(SettingsScope::Global);
    let mut parse_error = None;
    storage.with_lock(SettingsScope::Global, &mut |current| {
        let mut root = match current.map(parse_settings_json).transpose() {
            Ok(root) => root.unwrap_or_default(),
            Err(error) => { parse_error = Some(format!("Cannot persist the cursor-cli-oauth {action}: the settings file is unparseable ({error})")); return None; }
        };
        let mut provider = root.get("cursorCliOauthProvider").and_then(Value::as_object).cloned().unwrap_or_default();
        provider.extend(patch.clone()); root.insert("cursorCliOauthProvider".into(),Value::Object(provider));
        match serde_json::to_string_pretty(&root) {
            Ok(content) => Some(content), Err(error) => { parse_error = Some(error.to_string()); None }
        }
    })?;
    match parse_error { Some(error) => Err(error), None => Ok(()) }
}

pub fn persist_enabled(storage: &dyn maho_core::settings_manager::SettingsStorage, enabled: bool) -> Result<(),String> {
    persist_provider_patch(storage,"enabled state", &serde_json::Map::from_iter([("enabled".into(),Value::Bool(enabled))]))
}
pub fn persist_no_approval_acknowledgement(storage: &dyn maho_core::settings_manager::SettingsStorage, at: &str) -> Result<(),String> {
    persist_provider_patch(storage,"acknowledgement", &serde_json::Map::from_iter([
        ("enabled".into(),Value::Bool(true)),("noApprovalAcknowledgedAt".into(),Value::String(at.into()))]))
}

#[derive(Default)]
pub struct SettingsLoader {
    cached: Option<(String,Value,Value)>,
}
impl SettingsLoader {
    pub fn load(&mut self, cwd: &str, agent_dir: &str, home: &str, environment: &BTreeMap<String,String>) -> CursorCliOauthProviderSettings {
        use maho_core::settings_manager::{FileSettingsStorage,SettingsScope,SettingsStorage,parse_settings_json,get_settings_path};
        let revision = |scope| maho_core::paths::get_file_content_revision(&get_settings_path(cwd,agent_dir,scope,home)).unwrap_or_else(|| "missing".into());
        let key = format!("{cwd}|{}|{}",revision(SettingsScope::Global),revision(SettingsScope::Project));
        if self.cached.as_ref().is_none_or(|(cached,_,_)| cached != &key) {
            let storage = FileSettingsStorage::new(cwd,agent_dir,home);
            let read = |scope| {
                storage.select_source(scope);
                let mut value = Value::Null;
                let result = storage.with_lock(scope,&mut |content| {
                    if let Some(content) = content && let Ok(root) = parse_settings_json(content) {
                        value = root.get("cursorCliOauthProvider").cloned().unwrap_or(Value::Null);
                    }
                    None
                });
                if result.is_err() { value = Value::Null; }
                value
            };
            self.cached = Some((key,read(SettingsScope::Global),read(SettingsScope::Project)));
        }
        match &self.cached {
            Some((_,global,project)) => resolve_settings(&[global.clone(),project.clone()],environment),
            None => resolve_settings(&[],environment),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn env(items: &[(&str,&str)]) -> BTreeMap<String,String> { items.iter().map(|(k,v)| (k.to_string(),v.to_string())).collect() }
    #[test]
    fn all_defaults() { assert_eq!(parse_cursor_cli_oauth_provider_settings(&Value::Null,&BTreeMap::new()), CursorCliOauthProviderSettings::default()); }
    #[test]
    fn exact_settings_fields() {
        let settings = parse_cursor_cli_oauth_provider_settings(&json!({"enabled":true,"executablePath":"/settings/cursor-agent","forceExecution":false,
            "noApprovalAcknowledgedAt":"2026-08-17T10:30:00.000Z","executionMode":"plan","resumeMode":"off","pinnedAccount":"work",
            "contextRecapOnModelSwitch":false,"modelCatalogTtlHours":6,"sandboxMode":"proven","denyCommands":["rm -rf /"]}),&BTreeMap::new());
        assert!(settings.enabled); assert!(!settings.explicitly_disabled); assert!(!settings.force_execution);
        assert_eq!(settings.execution_mode,ExecutionMode::Plan); assert_eq!(settings.resume_mode,ResumeMode::Off);
        assert_eq!(settings.pinned_account.as_deref(),Some("work")); assert!(!settings.context_recap_on_model_switch);
        assert_eq!(settings.no_approval_acknowledged_at.as_deref(),Some("2026-08-17T10:30:00.000Z"));
    }
    #[test]
    fn environment_wins() {
        let settings = parse_cursor_cli_oauth_provider_settings(&json!({"enabled":false,"forceExecution":true,"executionMode":"agent"}),
            &env(&[("SENPI_CURSOR_CLI_OAUTH_ENABLED","TRUE"),("SENPI_CURSOR_CLI_OAUTH_FORCE","0"),("SENPI_CURSOR_CLI_OAUTH_EXECUTION_MODE","plan"),
                ("SENPI_CURSOR_CLI_OAUTH_MODEL_CATALOG_TTL_HOURS","12.5")]));
        assert!(settings.enabled); assert!(!settings.explicitly_disabled); assert!(!settings.force_execution);
        assert_eq!(settings.execution_mode,ExecutionMode::Plan); assert!((settings.model_catalog_ttl_hours-12.5).abs()<f64::EPSILON);
    }
    #[test]
    fn exact_boolean_environment_values() {
        for (value,expected) in [("1",true),("0",false),("true",true),("false",false),("TRUE",true),("False",false)] {
            assert_eq!(parse_cursor_cli_oauth_provider_settings(&Value::Null,&env(&[("SENPI_CURSOR_CLI_OAUTH_ENABLED",value)])).enabled,expected);
        }
    }
    #[test]
    fn invalid_environment_does_not_mask_disk() {
        let settings = parse_cursor_cli_oauth_provider_settings(&json!({"enabled":true,"forceExecution":false,"executionMode":"plan","modelCatalogTtlHours":8}),
            &env(&[("SENPI_CURSOR_CLI_OAUTH_ENABLED","yes"),("SENPI_CURSOR_CLI_OAUTH_FORCE","no"),("SENPI_CURSOR_CLI_OAUTH_EXECUTION_MODE","ask"),("SENPI_CURSOR_CLI_OAUTH_MODEL_CATALOG_TTL_HOURS","NaN")]));
        assert!(settings.enabled); assert!(!settings.force_execution); assert_eq!(settings.execution_mode,ExecutionMode::Plan);
        assert!((settings.model_catalog_ttl_hours-8.0).abs()<f64::EPSILON);
    }
    #[test]
    fn malformed_blocks_tolerated() {
        for value in [Value::Null,json!(true),json!(42),json!("settings"),json!([]),json!({"enabled":"true"})] {
            assert_eq!(parse_cursor_cli_oauth_provider_settings(&value,&BTreeMap::new()),CursorCliOauthProviderSettings::default());
        }
    }
    #[test]
    fn executable_environment_precedence() {
        assert_eq!(parse_cursor_cli_oauth_provider_settings(&Value::Null,&env(&[("CURSOR_AGENT_EXECUTABLE","fallback")])).executable_path.as_deref(),Some("fallback"));
        assert_eq!(parse_cursor_cli_oauth_provider_settings(&Value::Null,&env(&[("CURSOR_AGENT_EXECUTABLE","fallback"),("SENPI_CURSOR_CLI_OAUTH_EXECUTABLE","specific")])).executable_path.as_deref(),Some("specific"));
    }
    #[test]
    fn exact_commands_trimmed() {
        let settings = parse_cursor_cli_oauth_provider_settings(&json!({"denyCommands":["rm -rf /",42,"","  git push --force  ",null]}),&BTreeMap::new());
        assert_eq!(settings.deny_commands,["rm -rf /","git push --force"]);
        let settings = parse_cursor_cli_oauth_provider_settings(&Value::Null,&env(&[("SENPI_CURSOR_CLI_OAUTH_DENY_COMMANDS","echo one , echo two,,echo  three  ")]));
        assert_eq!(settings.deny_commands,["echo one","echo two","echo  three"]);
    }
    #[test]
    fn malformed_commands_ignored() {
        for value in [json!("rm -rf /"),json!({}),json!([])] {
            assert!(parse_cursor_cli_oauth_provider_settings(&json!({"denyCommands":value}),&BTreeMap::new()).deny_commands.is_empty());
        }
    }
    #[test]
    fn sandbox_warns_once_each() {
        let mut warnings = Vec::new();
        { let mut validate = create_sandbox_mode_validator(BTreeSet::from(["proven".into()]),|message| warnings.push(message));
          assert_eq!(validate(Some("proven")),Some("proven".into())); assert_eq!(validate(Some("unknown")),None);
          assert_eq!(validate(Some("unknown")),None); assert_eq!(validate(Some("another")),None); }
        assert_eq!(warnings.len(),2);
    }
    #[test]
    fn last_named_disabled_layer_wins() {
        assert!(resolve_settings(&[json!({"enabled":true}),json!({"enabled":false})],&BTreeMap::new()).explicitly_disabled);
        assert!(!resolve_settings(&[json!({"enabled":false}),json!({"enabled":true})],&BTreeMap::new()).explicitly_disabled);
        assert!(!resolve_settings(&[json!({})],&BTreeMap::new()).explicitly_disabled);
    }
    #[test]
    fn acknowledgement_preserves_siblings() {
        use maho_core::settings_manager::{InMemorySettingsStorage,SettingsStorage,SettingsScope};
        let storage = InMemorySettingsStorage::default();
        storage.with_lock(SettingsScope::Global,&mut |_| Some(json!({"theme":"dark","cursorCliOauthProvider":{"pinnedAccount":"work"}}).to_string())).unwrap();
        persist_no_approval_acknowledgement(&storage,"2026-08-17T12:00:00.000Z").unwrap();
        let stored: Value = serde_json::from_str(&storage.content(SettingsScope::Global).unwrap()).unwrap();
        assert_eq!(stored["theme"],"dark"); assert_eq!(stored["cursorCliOauthProvider"]["pinnedAccount"],"work");
        assert_eq!(stored["cursorCliOauthProvider"]["enabled"],true);
    }
    #[test]
    fn acknowledgement_creates_missing_block() {
        use maho_core::settings_manager::{InMemorySettingsStorage,SettingsScope};
        let storage = InMemorySettingsStorage::default();
        persist_no_approval_acknowledgement(&storage,"2026-08-17T12:05:00.000Z").unwrap();
        let stored: Value = serde_json::from_str(&storage.content(SettingsScope::Global).unwrap()).unwrap();
        assert_eq!(stored,json!({"cursorCliOauthProvider":{"enabled":true,"noApprovalAcknowledgedAt":"2026-08-17T12:05:00.000Z"}}));
    }
    #[test]
    fn acknowledgement_refuses_unparseable_content() {
        use maho_core::settings_manager::{InMemorySettingsStorage,SettingsStorage,SettingsScope};
        let storage = InMemorySettingsStorage::default();
        storage.with_lock(SettingsScope::Global,&mut |_| Some("{ not json".into())).unwrap();
        assert!(persist_no_approval_acknowledgement(&storage,"2026-08-17T12:00:00.000Z").is_err());
        assert_eq!(storage.content(SettingsScope::Global).as_deref(),Some("{ not json"));
    }
    #[test]
    fn disk_rewrite_invalidates_cache_and_env_is_live() {
        use maho_core::settings_manager::FileSettingsStorage;
        let directory = tempfile::tempdir().unwrap(); let path = directory.path().to_str().unwrap();
        let storage = FileSettingsStorage::new(path,path,path);
        persist_provider_patch(&storage,"test",json!({"enabled":true,"pinnedAccount":"first"}).as_object().unwrap()).unwrap();
        let mut loader = SettingsLoader::default();
        assert_eq!(loader.load(path,path,path,&BTreeMap::new()).pinned_account.as_deref(),Some("first"));
        assert_eq!(loader.load(path,path,path,&env(&[("SENPI_CURSOR_CLI_OAUTH_PINNED_ACCOUNT","live")])).pinned_account.as_deref(),Some("live"));
        persist_provider_patch(&storage,"test",json!({"enabled":false,"pinnedAccount":"second"}).as_object().unwrap()).unwrap();
        let changed = loader.load(path,path,path,&BTreeMap::new());
        assert_eq!(changed.pinned_account.as_deref(),Some("second")); assert!(changed.explicitly_disabled);
    }
}
