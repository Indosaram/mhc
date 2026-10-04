use std::sync::Arc;
use maho_ext_api::{ExtensionApi,NotificationType};
use crate::{prompts::{COLLAPSE_RULE_NAME,CONTROL_LEAK_RULE_NAME},types::*};
pub struct TtsrPublicState { pub rules:Vec<TtsrRule>,pub injected_rule_names:Vec<String>,pub disabled:bool }
pub fn register_ttsr_commands(api:&mut ExtensionApi,get_state:Arc<dyn Fn()->TtsrPublicState+Send+Sync>) {
    api.register_command("ttsr",Some("Show TTSR stream-rule status: builtin detectors, user rules, injected rules.".into()),None,Arc::new(move |_,ctx| { let get_state=Arc::clone(&get_state); Box::pin(async move { ctx.ui.notify(&format_ttsr_status(&get_state()),NotificationType::Info); Ok(()) }) }));
}
pub fn format_scope(scope:&TtsrScope)->String {
    let mut parts=Vec::new();
    if scope.allow_text { parts.push("text".into()); }
    if scope.allow_thinking { parts.push("thinking".into()); }
    for tool in &scope.tool_scopes { parts.push(tool.path_glob.as_ref().map_or_else(||format!("tool:{}",tool.tool_name),|glob|format!("tool:{}({glob})",tool.tool_name))); }
    if parts.is_empty() { "none".into() } else { parts.join("+") }
}
pub fn format_ttsr_status(state:&TtsrPublicState)->String {
    let builtin=state.rules.iter().filter(|rule|rule.source==RuleSource::Builtin).collect::<Vec<_>>(); let user=state.rules.iter().filter(|rule|rule.source!=RuleSource::Builtin).collect::<Vec<_>>();
    let mut lines=vec!["TTSR stream rules".into(),String::new(),"STATUS".into(),if state.disabled { "disabled (ttsr-disabled flag set)" } else { "enabled" }.into(),String::new(),"BUILTIN RULES".into(),"DETECTORS".into(),format!("{COLLAPSE_RULE_NAME} [detector: collapse] remediation: abort the stream, truncate the garbage from history, inject a corrective nudge, then continue (mode: nudge, scope: output-region)"),format!("{CONTROL_LEAK_RULE_NAME} [detector: control-leak] remediation: abort the stream, replace the generation with an error shell, then resample via bounded provider retry (mode: provider-error, scope: generation)"),"STREAM RULES".into()];
    if builtin.is_empty() { lines.push("(none)".into()); } else { lines.extend(builtin.into_iter().map(|rule|format!("{} [stream rule, scope: {}]",rule.name,format_scope(&rule.scope)))); }
    lines.extend([String::new(),"USER RULES".into()]);
    if user.is_empty() { lines.push("(none)".into()); } else { lines.extend(user.into_iter().map(|rule|format!("{} [{}, scope: {}]",rule.name,match rule.source { RuleSource::Builtin=>"builtin",RuleSource::Project=>"project",RuleSource::Global=>"global" },format_scope(&rule.scope)))); }
    lines.extend([String::new(),"INJECTED".into(),if state.injected_rule_names.is_empty() { "(none)".into() } else { state.injected_rule_names.join(", ") }]); lines.join("\n")
}
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn command_registers_on_native_api() { let mut api=ExtensionApi::new(maho_ext_api::LoadedExtension::new("ttsr",std::path::PathBuf::from("."),Default::default()),Default::default(),Default::default(),Default::default()); register_ttsr_commands(&mut api,Arc::new(||TtsrPublicState { rules:vec![],injected_rule_names:vec![],disabled:false })); assert_eq!(api.registered.commands.len(),1); assert_eq!(api.registered.commands[0].name,"ttsr"); }
}
