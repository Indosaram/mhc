use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use crate::types::{ExecutableHookHandler, HookSourceMetadata, HookSourceScope, HookTrustEntry, HookTrustState};

#[derive(Debug, thiserror::Error)]
pub enum TrustError {
    #[error("Invalid command hook timeout reached trust hashing.")]
    InvalidTimeout,
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[derive(Clone, Debug, PartialEq)]
pub struct HookTrustRecord {
    pub id: String,
    pub current_hash: String,
    pub enabled: bool,
    pub trusted: bool,
    pub executable: bool,
    pub scope: HookSourceScope,
    pub source_path: String,
    pub matcher: Option<String>,
    pub command_preview: String,
    pub entry: Option<HookTrustEntry>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookTrustStorageScope { Global, Project }

fn sha256_hex(value: &str) -> String { format!("{:x}",Sha256::digest(value.as_bytes())) }

fn source_key_hash(source: &HookSourceMetadata) -> String {
    let scope = match source.scope {
        HookSourceScope::Global => "global", HookSourceScope::Project => "project",
        HookSourceScope::Plugin => "plugin", HookSourceScope::Runtime => "runtime",
        HookSourceScope::Cli => "cli", HookSourceScope::Managed => "managed",
    };
    let value = [scope, &source.source_path, source.plugin_root.as_deref().unwrap_or(""), source.manifest_path.as_deref().unwrap_or("")].join("\0");
    sha256_hex(&value)[..12].to_owned()
}

pub fn hook_trust_id(handler: &ExecutableHookHandler) -> String {
    format!("hk_{}_{}_{}_{}",source_key_hash(&handler.source),handler.event.as_str(),handler.group_index,handler.handler_index)
}

fn selected_command<'a>(handler: &'a ExecutableHookHandler, platform: &str) -> &'a str {
    if platform == "win32" { handler.config.command_windows.as_deref().unwrap_or(&handler.config.command) }
    else { &handler.config.command }
}

pub fn hash_command_hook(handler: &ExecutableHookHandler, platform: &str) -> Result<String, TrustError> {
    let timeout = handler.config.timeout.unwrap_or(600.0);
    if !timeout.is_finite() || timeout <= 0.0 { return Err(TrustError::InvalidTimeout); }
    let timeout = if timeout.fract() == 0.0 {
        // JavaScript JSON.stringify writes integral numbers without a decimal suffix.
        serde_json::from_str::<Value>(&format!("{timeout:.0}"))?
    } else { json!(timeout) };
    let mut hook = Map::new();
    hook.insert("async".to_owned(),json!(false));
    hook.insert("command".to_owned(),json!(handler.config.command));
    if let Some(value) = &handler.config.command_windows { hook.insert("commandWindows".to_owned(),json!(value)); }
    hook.insert("platformCommand".to_owned(),json!(selected_command(handler,platform)));
    if let Some(value) = &handler.config.status_message { hook.insert("statusMessage".to_owned(),json!(value)); }
    hook.insert("timeout".to_owned(),timeout);
    hook.insert("type".to_owned(),json!("command"));
    let mut identity = Map::new();
    identity.insert("event".to_owned(),json!(handler.event));
    identity.insert("hook".to_owned(),Value::Object(hook));
    if let Some(value) = &handler.matcher { identity.insert("matcher".to_owned(),json!(value)); }
    identity.insert("sourceKeyHash".to_owned(),json!(source_key_hash(&handler.source)));
    Ok(format!("sha256:{}",sha256_hex(&serde_json::to_string(&identity)?)))
}

pub fn build_hook_trust_record(handler: &ExecutableHookHandler, platform: &str) -> Result<HookTrustRecord, TrustError> {
    Ok(HookTrustRecord {
        id: hook_trust_id(handler), current_hash: hash_command_hook(handler,platform)?,
        enabled: true, trusted: false, executable: false, scope: handler.source.scope.clone(),
        source_path: handler.source.source_path.clone(), matcher: handler.matcher.clone(),
        command_preview: selected_command(handler,platform).to_owned(), entry: None,
    })
}

pub fn create_hook_trust_entry(handler: &ExecutableHookHandler, platform: &str, updated_at: &str) -> Result<HookTrustEntry, TrustError> {
    Ok(HookTrustEntry { enabled: true, trusted_hash: Some(hash_command_hook(handler,platform)?),
        scope: handler.source.scope.clone(), source_path: handler.source.source_path.clone(),
        matcher: handler.matcher.clone(), command_preview: selected_command(handler,platform).to_owned(), updated_at: updated_at.to_owned(),
    })
}

fn build_stateful_hook_trust_record(handler: &ExecutableHookHandler, state: &HookTrustState, platform: &str) -> Result<HookTrustRecord, TrustError> {
    let mut record = build_hook_trust_record(handler,platform)?;
    if let Some(entry) = state.hooks.get(&record.id) {
        record.enabled = entry.enabled;
        record.trusted = entry.trusted_hash.as_deref() == Some(&record.current_hash);
        record.entry = Some(entry.clone());
    }
    record.executable = record.enabled && record.trusted;
    Ok(record)
}

pub fn is_command_hook_trusted(handler: &ExecutableHookHandler, state: &HookTrustState, platform: &str) -> Result<bool, TrustError> {
    Ok(build_stateful_hook_trust_record(handler,state,platform)?.executable)
}

pub fn list_hook_trust_records(handlers: &[ExecutableHookHandler], state: &HookTrustState, platform: &str) -> Result<Vec<HookTrustRecord>, TrustError> {
    handlers.iter().map(|h|build_stateful_hook_trust_record(h,state,platform)).collect()
}

pub fn filter_executable_trusted_hooks<'a>(handlers: &'a [ExecutableHookHandler], state: &HookTrustState, platform: &str) -> Result<Vec<&'a ExecutableHookHandler>, TrustError> {
    let mut executable = Vec::new();
    for handler in handlers { if is_command_hook_trusted(handler,state,platform)? { executable.push(handler); } }
    Ok(executable)
}

pub fn hook_trust_storage_scope(handler: &ExecutableHookHandler, project_trusted: bool) -> Option<HookTrustStorageScope> {
    match handler.source.scope {
        HookSourceScope::Project => project_trusted.then_some(HookTrustStorageScope::Project),
        HookSourceScope::Global | HookSourceScope::Plugin | HookSourceScope::Runtime | HookSourceScope::Cli | HookSourceScope::Managed => Some(HookTrustStorageScope::Global),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trust_state_json::{empty_hook_trust_state,read_hook_trust_state_json};
    use crate::types::{CommandHookConfig,HookDiscoveryTiming,SupportedHookEvent};

    fn handler() -> ExecutableHookHandler {
        ExecutableHookHandler { event: SupportedHookEvent::PreToolUse, matcher: Some("Bash".to_owned()), group_index: 0, handler_index: 0,
            config: CommandHookConfig { kind:"command".to_owned(), command:"node hooks/check.mjs".to_owned(), command_windows:Some("node hooks/check.ps1".to_owned()),timeout:Some(30.0),status_message:Some("Checking tool call".to_owned()) },
            source:HookSourceMetadata { scope:HookSourceScope::Project,source_path:"/repo/.senpi/hooks.json".to_owned(),display_order:1,discovered_at:HookDiscoveryTiming::PreSession,plugin_root:None,manifest_path:None,plugin_env:None },
        }
    }

    #[test]
    fn stable_ids_and_canonical_hash_match_upstream() -> Result<(),TrustError> {
        let mut hook = handler(); hook.group_index=2;hook.handler_index=3;
        let record=build_hook_trust_record(&hook,"linux")?;
        assert_eq!(record.id,"hk_f426e074193a_PreToolUse_2_3");
        assert_eq!(record.current_hash,"sha256:83901748237ea5ed4ec8741e50e7fc233770daf0e7f804f4300013ffa44d0406");
        assert_eq!(record.command_preview,"node hooks/check.mjs");
        assert_ne!(hash_command_hook(&hook,"win32")?,record.current_hash);
        Ok(())
    }

    #[test]
    fn identity_changes_invalidate_trust() -> Result<(),TrustError> {
        let hook=handler();let mut state=empty_hook_trust_state();state.hooks.insert(hook_trust_id(&hook),create_hook_trust_entry(&hook,"linux","fixed")?);
        assert!(is_command_hook_trusted(&hook,&state,"linux")?);
        for field in 0..4 {
            let mut changed=hook.clone();
            match field { 0=>changed.config.command="node changed".to_owned(),1=>changed.config.command_windows=Some("changed.ps1".to_owned()),2=>changed.config.timeout=Some(31.0),3=>changed.config.status_message=Some("Different status".to_owned()),_=>unreachable!() }
            assert!(!is_command_hook_trusted(&changed,&state,"linux")?);
        }
        Ok(())
    }

    #[test]
    fn invalid_timeouts_fail_before_hashing() {
        for timeout in [0.0,-1.0,f64::NAN,f64::INFINITY,f64::NEG_INFINITY] {
            let mut hook=handler();hook.config.timeout=Some(timeout);
            assert!(matches!(build_hook_trust_record(&hook,"linux"),Err(TrustError::InvalidTimeout)));
        }
    }

    #[test]
    fn disabled_and_untrusted_hooks_are_listed_but_not_executed() -> Result<(),TrustError> {
        let mut hooks=vec![handler();3];for (i,hook) in hooks.iter_mut().enumerate() {hook.handler_index=i;}
        let mut state=empty_hook_trust_state();
        for hook in &hooks[..2] {let mut entry=create_hook_trust_entry(hook,"linux","fixed")?;entry.enabled=hook.handler_index==0;state.hooks.insert(hook_trust_id(hook),entry);}
        let records=list_hook_trust_records(&hooks,&state,"linux")?;
        assert_eq!(records.iter().map(|r|(r.enabled,r.executable,r.trusted)).collect::<Vec<_>>(),[(true,true,true),(false,false,true),(true,false,false)]);
        assert_eq!(filter_executable_trusted_hooks(&hooks,&state,"linux")?,vec![&hooks[0]]);Ok(())
    }

    #[test]
    fn malformed_stale_states_remain_untrusted() -> Result<(),TrustError> {
        for input in ["{ bad json",r#"{"version":0,"hooks":{}}"#] {assert!(!is_command_hook_trusted(&handler(),&read_hook_trust_state_json(Some(input)),"linux")?);}Ok(())
    }

    #[test]
    fn project_storage_requires_project_trust() {
        let mut hook=handler();assert_eq!(hook_trust_storage_scope(&hook,false),None);assert_eq!(hook_trust_storage_scope(&hook,true),Some(HookTrustStorageScope::Project));
        hook.source.scope=HookSourceScope::Global;assert_eq!(hook_trust_storage_scope(&hook,false),Some(HookTrustStorageScope::Global));
    }

    #[test]
    fn trusted_entry_round_trips_through_file_storage() -> std::io::Result<()> {
        let dir=tempfile::tempdir()?;
        let hook=handler();
        let storage=crate::trust_storage::FileHookStateStorage::new(dir.path(),dir.path());
        let entry=create_hook_trust_entry(&hook,"linux","fixed").map_err(std::io::Error::other)?;
        storage.update(HookTrustStorageScope::Project,|mut state| {state.hooks.insert(hook_trust_id(&hook),entry.clone());state})?;
        let state=storage.read(HookTrustStorageScope::Project)?;
        assert!(is_command_hook_trusted(&hook,&state,"linux").map_err(std::io::Error::other)?);
        assert!(!is_command_hook_trusted(&hook,&storage.read(HookTrustStorageScope::Global)?,"linux").map_err(std::io::Error::other)?);
        Ok(())
    }
}
