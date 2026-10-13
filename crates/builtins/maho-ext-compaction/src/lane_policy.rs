use maho_ai::types::Message;
use serde_json::{Map, Value};

pub const ANTHROPIC_SUBSCRIPTION_COMPACT_ENTRY_TYPE: &str = "claude-sdk-oauth-compact";
pub const ANTHROPIC_SUBSCRIPTION_COMPACT_BOUNDARY_DIAGNOSTIC: &str = "claude_sdk_oauth_compact_boundary";
pub const SDK_NATIVE_LANE_REJECTION_REASON: &str = "the Claude Agent SDK owns compaction for this session";
pub const COMPACT_BOUNDARY_SCHEMA: &str = "senpi.claude-sdk-oauth.compact-boundary.v1";

pub fn is_sdk_native_compaction_lane(provider: Option<&str>, resume_mode: Option<&str>) -> bool {
    provider == Some("anthropic-subscription") && resume_mode != Some("off")
}
#[derive(Clone, Debug, Default)]
pub struct CompactionLanePolicy { cached_cwd: Option<String>, cached_resume_mode: Option<String> }
impl CompactionLanePolicy {
    pub fn disables_senpi_compaction<E>(
        &mut self, cwd: &str, provider: Option<&str>, compaction_model: Option<&str>,
        mut load: impl FnMut(&str) -> Result<Option<String>, E>,
    ) -> bool {
        if provider != Some("anthropic-subscription") || compaction_model.is_some_and(|m| !m.is_empty()) { return false; }
        if self.cached_cwd.as_deref() != Some(cwd) {
            match load(cwd) {
                Ok(mode) => { self.cached_resume_mode = mode; self.cached_cwd = Some(cwd.into()); }
                Err(_) => { self.cached_cwd = None; return false; }
            }
        }
        is_sdk_native_compaction_lane(provider, self.cached_resume_mode.as_deref())
    }
    pub fn owns_compaction<E>(
        &mut self, cwd: &str, provider: Option<&str>, compaction_model: Option<&str>, reason: &str,
        load: impl FnMut(&str) -> Result<Option<String>, E>,
    ) -> bool {
        reason == "manual" || !self.disables_senpi_compaction(cwd, provider, compaction_model, load)
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
        Ok(settings.values.get("resumeMode").and_then(Value::as_str).map(str::to_owned))
    })
}
/// Live `lanePolicy.ownsCompaction(ctx, reason)`: manual requests stay senpi-owned on every lane.
pub fn live_owns_compaction(policy: &std::sync::Mutex<CompactionLanePolicy>, context: &maho_ext_api::ExtensionContext, reason: maho_ext_api::CompactionReason) -> bool {
    reason == maho_ext_api::CompactionReason::Manual || !live_disables_senpi_compaction(policy, context)
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
