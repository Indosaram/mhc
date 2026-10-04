use crate::settings::{CursorCliOauthProviderSettings, ExecutionMode};
use std::{collections::BTreeSet, path::Path};
use serde_json::{Value,json};

pub const SANDBOX_MODES: [&str;2] = ["enabled","disabled"];
pub const NO_APPROVAL_EXPLANATION: &str = "The Cursor CLI executes its own tools autonomously: with --force there is no senpi approval, no senpi sandboxing, and no tool-level audit for what it runs.";
pub const ACKNOWLEDGEMENT_STEP: &str = "Set \"cursorCliOauthProvider.noApprovalAcknowledgedAt\" to the current ISO-8601 timestamp (for example \"2026-08-17T12:00:00.000Z\") in senpi settings to acknowledge this once.";
pub const FORCE_DISABLED_WARNING: &str = "cursor-cli-oauth: forceExecution is disabled in agent mode, so the Cursor CLI will auto-reject every tool call; set executionMode to \"plan\" for planning turns that do not need execution.";

#[derive(Clone,Debug,PartialEq,Eq)]
pub struct GuardrailWarning { pub code: &'static str, pub message: String }
#[derive(Default)]
pub struct GuardrailSession { pub warnings: Vec<GuardrailWarning>, emitted: BTreeSet<String> }
impl GuardrailSession {
    pub fn warn(&mut self, code: &'static str, message: String, key: Option<String>) -> bool {
        if !self.emitted.insert(key.unwrap_or_else(|| code.into())) { return false; }
        self.warnings.push(GuardrailWarning { code,message }); true
    }
    pub fn validate_sandbox_mode(&mut self, value: Option<&str>) -> Option<String> {
        let value = value?;
        if SANDBOX_MODES.contains(&value) { return Some(value.into()); }
        let message = format!("Ignoring unrecognized Cursor CLI OAuth sandbox mode: {value}");
        self.warn("sandbox_mode_ignored",message.clone(),Some(format!("sandbox_mode_ignored:{message}"))); None
    }
}
#[derive(Clone,Debug,PartialEq,Eq)]
pub struct CursorCliExecutionRefusalError { pub code: &'static str, pub message: String, pub acknowledgement_step: &'static str }
impl std::fmt::Display for CursorCliExecutionRefusalError {
    fn fmt(&self,f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(&self.message) }
}
impl std::error::Error for CursorCliExecutionRefusalError {}
#[derive(Clone,Debug,PartialEq,Eq)]
pub struct ExecutionDecision {
    pub force: bool, pub execution_mode: ExecutionMode, pub sandbox_mode: Option<String>,
    pub deny_commands: Vec<String>, pub warnings: Vec<GuardrailWarning>,
}
pub fn force_refusal_pending(input: &CursorCliOauthProviderSettings) -> bool {
    input.execution_mode != ExecutionMode::Plan && input.force_execution && input.no_approval_acknowledged_at.is_none()
}
pub fn sanitize_deny_commands(entries: &[String], mut warn: impl FnMut(String)) -> Vec<String> {
    let mut kept = Vec::new();
    for entry in entries {
        let command = entry.trim();
        if command.is_empty() { warn("Ignoring an empty Cursor CLI deny command entry (exact full commands only)".into()); }
        else if command.contains(['*','?','[']) {
            warn(format!("Ignoring glob-bearing Cursor CLI deny command (exact full commands only; glob support is unproven): {command}"));
        } else { kept.push(command.into()); }
    }
    kept
}
pub fn resolve_execution_policy(input: &CursorCliOauthProviderSettings, session: &mut GuardrailSession, deny_commands: &[String]) -> Result<ExecutionDecision,CursorCliExecutionRefusalError> {
    if force_refusal_pending(input) {
        return Err(CursorCliExecutionRefusalError { code:"no_approval_acknowledgement_required",
            message: format!("{NO_APPROVAL_EXPLANATION} {ACKNOWLEDGEMENT_STEP}"), acknowledgement_step:ACKNOWLEDGEMENT_STEP });
    }
    let plan = input.execution_mode == ExecutionMode::Plan;
    if !plan && !input.force_execution { session.warn("force_execution_disabled",FORCE_DISABLED_WARNING.into(),None); }
    let sandbox_mode = session.validate_sandbox_mode(input.sandbox_mode.as_deref());
    let deny_commands = sanitize_deny_commands(deny_commands,|message| {
        session.warn("deny_command_rejected",message.clone(),Some(format!("deny_command_rejected:{message}")));
    });
    Ok(ExecutionDecision { force: !plan && input.force_execution, execution_mode:input.execution_mode,
        sandbox_mode,deny_commands,warnings:session.warnings.clone() })
}
pub fn apply_deny_config(home: &Path, deny_commands: &[String]) -> anyhow::Result<()> {
    if deny_commands.is_empty() { return Ok(()); }
    let path = home.join(".cursor/cli-config.json");
    let mut existing = match std::fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str::<Value>(&content).ok().and_then(|v| v.as_object().cloned()).unwrap_or_default(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => serde_json::Map::new(),
        Err(error) => return Err(error.into()),
    };
    let mut permissions = existing.get("permissions").and_then(Value::as_object).cloned().unwrap_or_default();
    permissions.insert("deny".into(),json!(deny_commands.iter().map(|command| format!("Shell({command})")).collect::<Vec<_>>()));
    existing.insert("permissions".into(),Value::Object(permissions));
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b"\t");
    let mut bytes = Vec::new();
    serde::Serialize::serialize(&existing,&mut serde_json::Serializer::with_formatter(&mut bytes,formatter))?;
    bytes.push(b'\n'); std::fs::write(path,bytes)?; Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn refuses_unacknowledged_force() {
        let error = resolve_execution_policy(&Default::default(),&mut Default::default(),&[]).unwrap_err();
        assert_eq!(error.code,"no_approval_acknowledgement_required"); assert_eq!(error.acknowledgement_step,ACKNOWLEDGEMENT_STEP);
    }
    #[test]
    fn acknowledged_force_allowed() {
        let input = CursorCliOauthProviderSettings { no_approval_acknowledged_at:Some("2026-08-17T00:00:00.000Z".into()),..Default::default() };
        assert!(resolve_execution_policy(&input,&mut Default::default(),&[]).unwrap().force);
    }
    #[test]
    fn plan_never_forces() {
        let input = CursorCliOauthProviderSettings { execution_mode:ExecutionMode::Plan,..Default::default() };
        assert!(!resolve_execution_policy(&input,&mut Default::default(),&[]).unwrap().force);
    }
    #[test]
    fn force_disabled_warns_once() {
        let input = CursorCliOauthProviderSettings { force_execution:false,..Default::default() }; let mut session = GuardrailSession::default();
        resolve_execution_policy(&input,&mut session,&[]).unwrap(); resolve_execution_policy(&input,&mut session,&[]).unwrap();
        assert_eq!(session.warnings.len(),1);
    }
    #[test]
    fn glob_and_empty_commands_deduplicate_warnings() {
        let input = CursorCliOauthProviderSettings { force_execution:false,..Default::default() }; let mut session = GuardrailSession::default();
        let entries = ["rm -rf /tmp/*","cat ~/.cache/[a-z]*.log","echo ?","echo safe","   ",""].map(str::to_owned);
        let decision = resolve_execution_policy(&input,&mut session,&entries).unwrap();
        assert_eq!(decision.deny_commands,["echo safe"]); assert_eq!(session.warnings.iter().filter(|w| w.code=="deny_command_rejected").count(),4);
    }
    #[test]
    fn sandbox_warns_once_per_unknown() {
        let mut session = GuardrailSession::default();
        assert_eq!(session.validate_sandbox_mode(Some("read-only")),None); assert_eq!(session.validate_sandbox_mode(Some("read-only")),None);
        assert_eq!(session.validate_sandbox_mode(Some("workspace-write")),None); assert_eq!(session.warnings.len(),2);
        for mode in SANDBOX_MODES { assert_eq!(session.validate_sandbox_mode(Some(mode)),Some(mode.into())); }
        assert_eq!(session.warnings.len(),2);
    }
    #[test]
    fn writes_exact_deny_shape_and_preserves_owned_keys() {
        let home = tempfile::tempdir().unwrap(); std::fs::create_dir(home.path().join(".cursor")).unwrap();
        apply_deny_config(home.path(),&["rm -rf /".into()]).unwrap();
        let path = home.path().join(".cursor/cli-config.json");
        let initial:Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(initial,json!({"permissions":{"deny":["Shell(rm -rf /)"]}}));
        std::fs::write(&path,json!({"theme":"dark","permissions":{"allow":["Shell(ls)"]}}).to_string()).unwrap();
        apply_deny_config(home.path(),&["rm -rf /".into()]).unwrap();
        let updated:Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(updated,json!({"theme":"dark","permissions":{"allow":["Shell(ls)"],"deny":["Shell(rm -rf /)"]}}));
    }
    #[test]
    fn empty_deny_commands_write_nothing() {
        let home = tempfile::tempdir().unwrap(); apply_deny_config(home.path(),&[]).unwrap();
        assert!(!home.path().join(".cursor/cli-config.json").exists());
    }
}
