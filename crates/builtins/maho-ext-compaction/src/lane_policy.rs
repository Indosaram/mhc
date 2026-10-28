use maho_ai::types::Message;
use serde_json::{Map, Value};

pub const ANTHROPIC_SUBSCRIPTION_COMPACT_ENTRY_TYPE: &str = "claude-sdk-oauth-compact";
pub const ANTHROPIC_SUBSCRIPTION_COMPACT_BOUNDARY_DIAGNOSTIC: &str = "claude_sdk_oauth_compact_boundary";
pub const SDK_NATIVE_LANE_REJECTION_REASON: &str = "the Claude Agent SDK owns compaction for this session";
pub const COMPACT_BOUNDARY_SCHEMA: &str = "senpi.claude-sdk-oauth.compact-boundary.v1";

pub fn is_sdk_native_compaction_lane(provider: Option<&str>, resume_mode: Option<&str>, compaction_owner: Option<&str>) -> bool {
    provider == Some("anthropic-subscription") && compaction_owner == Some("sdk") && resume_mode != Some("off")
}
#[derive(Clone, Debug, Default)]
pub struct CompactionLanePolicy { _private: () }
impl CompactionLanePolicy {
    pub fn disables_senpi_compaction<E>(
        &mut self, cwd: &str, provider: Option<&str>, compaction_model: Option<&str>,
        mut load: impl FnMut(&str) -> Result<(Option<String>, Option<String>), E>,
    ) -> bool {
        if provider != Some("anthropic-subscription") || compaction_model.is_some_and(|m| !m.is_empty()) { return false; }
        match load(cwd) { Ok((resume_mode, owner)) => is_sdk_native_compaction_lane(provider, resume_mode.as_deref(), owner.as_deref()), Err(_) => false }
    }
    pub fn owns_compaction<E>(
        &mut self, cwd: &str, provider: Option<&str>, compaction_model: Option<&str>, reason: &str,
        load: impl FnMut(&str) -> Result<(Option<String>, Option<String>), E>,
    ) -> bool {
        if reason == "manual" { return true; }
        if !self.disables_senpi_compaction(cwd, provider, compaction_model, load) { return true; }
        false
    }
}
/// Live `lanePolicy.disablesSenpiCompaction(ctx)`: reads the configured `compaction.model`
/// override and the resolved `claudeSdkOauthProvider.resumeMode` from senpi's settings files.
/// Settings are only touched when the active model belongs to the lane.
pub fn live_disables_senpi_compaction(policy: &std::sync::Mutex<CompactionLanePolicy>, context: &maho_ext_api::ExtensionContext) -> bool {
    let provider = context.model.as_ref().map(|model| model.provider.as_str());
    if provider != Some(maho_ext_anthropic_subscription::account_management::ANTHROPIC_SUBSCRIPTION_PROVIDER_ID) { return false; }
    let cwd = context.cwd.to_string_lossy();
    let manager = maho_core::settings_manager::SettingsManager::create(&cwd, &context.agent_dir.to_string_lossy(), &maho_core::config::home_dir(), (context.is_project_trusted_fn)());
    let compaction_model = manager.get_value("compaction").and_then(|compaction| compaction.get("model")).and_then(Value::as_str).map(str::to_owned);
    policy.lock().unwrap_or_else(std::sync::PoisonError::into_inner).disables_senpi_compaction(&cwd, provider, compaction_model.as_deref(), |_| {
        if let Some(error) = manager.global_settings_load_error().or(manager.project_settings_load_error()) { return Err(error.to_owned()); }
        let environment: std::collections::BTreeMap<String, String> = std::env::vars().collect();
        let settings = maho_ext_anthropic_subscription::settings::load(&Value::Object(manager.get_global().clone()), &Value::Object(manager.get_project().clone()), &environment);
        Ok((settings.values.get("resumeMode").and_then(Value::as_str).map(str::to_owned), settings.values.get("compactionOwner").and_then(Value::as_str).map(str::to_owned)))
    })
}

pub fn live_has_append_only_transcript(context: &maho_ext_api::ExtensionContext) -> bool {
    let provider = context.model.as_ref().map(|model| model.provider.as_str());
    if provider != Some(maho_ext_anthropic_subscription::account_management::ANTHROPIC_SUBSCRIPTION_PROVIDER_ID) { return false; }
    let cwd = context.cwd.to_string_lossy();
    let manager = maho_core::settings_manager::SettingsManager::create(&cwd, &context.agent_dir.to_string_lossy(), &maho_core::config::home_dir(), (context.is_project_trusted_fn)());
    if manager.global_settings_load_error().is_some() || manager.project_settings_load_error().is_some() { return false; }
    let environment: std::collections::BTreeMap<String, String> = std::env::vars().collect();
    let settings = maho_ext_anthropic_subscription::settings::load(&Value::Object(manager.get_global().clone()), &Value::Object(manager.get_project().clone()), &environment);
    settings.values.get("resumeMode").and_then(Value::as_str) != Some("off")
}
pub const COLD_SEED_OVERFLOW_DIAGNOSTIC: &str = "claude_sdk_oauth_cold_seed_overflow";

/// senpi `isColdSeedOverflowMessage`: an assistant message carrying the cold-seed overflow diagnostic.
pub fn is_cold_seed_overflow_message(message: &Value) -> bool {
    message.get("role").and_then(Value::as_str) == Some("assistant")
        && message.get("diagnostics").and_then(Value::as_array).is_some_and(|diagnostics| {
            diagnostics.iter().any(|d| d.get("type").and_then(Value::as_str) == Some(COLD_SEED_OVERFLOW_DIAGNOSTIC))
        })
}

/// senpi `lastTurnIsColdSeedOverflow`: only the newest assistant since the latest compaction counts.
/// `branch` holds raw session entries (`type`, `message`).
pub fn last_turn_is_cold_seed_overflow(branch: &[Value]) -> bool {
    for entry in branch.iter().rev() {
        match entry.get("type").and_then(Value::as_str) {
            Some("compaction") => return false,
            Some("message") if entry.get("message").and_then(|m| m.get("role")).and_then(Value::as_str) == Some("assistant") => {
                return entry.get("message").is_some_and(is_cold_seed_overflow_message);
            }
            _ => {}
        }
    }
    false
}

/// Live `lanePolicy.ownsCompaction(ctx, reason)`: manual requests stay senpi-owned on every lane;
/// overflow is senpi's only when the newest turn is a failed cold-seed.
pub fn live_owns_compaction(policy: &std::sync::Mutex<CompactionLanePolicy>, context: &maho_ext_api::ExtensionContext, reason: maho_ext_api::CompactionReason) -> bool {
    if !live_disables_senpi_compaction(policy, context) || reason == maho_ext_api::CompactionReason::Manual { return true; }
    reason == maho_ext_api::CompactionReason::Overflow && {
        let branch: Vec<Value> = context.session_manager.get_branch().into_iter().map(|entry| {
            let mut value = entry.data;
            value["type"] = Value::String(entry.kind);
            value
        }).collect();
        last_turn_is_cold_seed_overflow(&branch)
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct CompactBoundaryEntry {
    pub schema: &'static str,
    pub sdk_session_id: String,
    pub uuid: String,
    pub compact_metadata: Map<String, Value>,
}
pub fn parse_compact_boundary_message(value: &Value) -> Option<CompactBoundaryEntry> {
    let record = value.as_object()?;
    if record.get("type")?.as_str()? != "system" || record.get("subtype")?.as_str()? != "compact_boundary" { return None; }
    Some(CompactBoundaryEntry {
        schema: COMPACT_BOUNDARY_SCHEMA,
        sdk_session_id: record.get("session_id")?.as_str()?.into(),
        uuid: record.get("uuid")?.as_str()?.into(),
        compact_metadata: record.get("compact_metadata")?.as_object()?.clone(),
    })
}
pub fn collect_compact_boundary_entries(message: &Message) -> Vec<CompactBoundaryEntry> {
    let Message::Assistant(assistant) = message else { return Vec::new(); };
    assistant.diagnostics.iter().flatten().filter(|d| d.kind == ANTHROPIC_SUBSCRIPTION_COMPACT_BOUNDARY_DIAGNOSTIC)
        .filter_map(|d| d.details.as_ref().and_then(|details| parse_compact_boundary_message(&Value::Object(details.clone())))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sdk_ownership_is_explicit_and_resume_mode_can_disable_it() {
        assert!(is_sdk_native_compaction_lane(Some("anthropic-subscription"), Some("auto"), Some("sdk")));
        assert!(!is_sdk_native_compaction_lane(Some("anthropic-subscription"), Some("auto"), None));
        assert!(!is_sdk_native_compaction_lane(Some("anthropic-subscription"), Some("off"), Some("sdk")));
        assert!(!is_sdk_native_compaction_lane(Some("openai"), Some("auto"), Some("sdk")));
    }

    #[test]
    fn overflow_ownership_requires_an_unsettled_cold_seed_overflow() {
        use serde_json::json;
        let failed = json!({"type":"message","message":{"role":"assistant","diagnostics":[{"type":COLD_SEED_OVERFLOW_DIAGNOSTIC}]}});
        let plain = json!({"type":"message","message":{"role":"assistant"}});
        let user = json!({"type":"message","message":{"role":"user"}});
        let compaction = json!({"type":"compaction"});
        assert!(last_turn_is_cold_seed_overflow(&[plain.clone(), failed.clone(), user.clone()]));
        assert!(!last_turn_is_cold_seed_overflow(&[failed.clone(), plain.clone()]));
        assert!(!last_turn_is_cold_seed_overflow(&[failed.clone(), compaction]));
        assert!(!last_turn_is_cold_seed_overflow(&[]));
        assert!(!is_cold_seed_overflow_message(&json!({"role":"user","diagnostics":[{"type":COLD_SEED_OVERFLOW_DIAGNOSTIC}]})));
    }

    #[test]
    fn settings_are_reloaded_and_manual_ownership_is_preserved() {
        let mut policy = CompactionLanePolicy::default();
        let mut loads = 0;
        for _ in 0..2 {
            assert!(policy.disables_senpi_compaction("/repo", Some("anthropic-subscription"), None, |_| {
                loads += 1;
                Ok::<_, ()>((Some("auto".into()), Some("sdk".into())))
            }));
        }
        assert_eq!(loads, 2);
        assert!(policy.owns_compaction("/repo", Some("anthropic-subscription"), None, "manual", |_| -> Result<(Option<String>, Option<String>), ()> {
            panic!("manual ownership must not read settings")
        }));
        assert!(!policy.owns_compaction("/repo", Some("anthropic-subscription"), None, "threshold", |_| {
            Ok::<_, ()>((Some("auto".into()), Some("sdk".into())))
        }));
    }
}
