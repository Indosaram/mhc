pub const HOOKS_USAGE:&str="Usage: /hooks [list|diagnostics|trust <id>|disable <id>|enable <id>|reload]";
pub const HOOK_SUBCOMMANDS:[&str;6]=["list","diagnostics","trust","disable","enable","reload"];
pub fn register_hooks_command(api:&mut maho_ext_api::types::ExtensionApi) {
    use maho_ext_api::types::{ExtensionFailure,NotificationType};use std::sync::Arc;
    api.register_command_with_context_and_completions("hooks",Some("Inspect loaded builtin hook sources and diagnostics.".to_owned()),None,Arc::new(|args,ctx|Box::pin(async move {
        let command=parse_hooks_command(args);
        if command==HookCommand::Usage {ctx.ui.notify(HOOKS_USAGE,NotificationType::Error);return Ok(());}
        if command==HookCommand::Reload {ctx.reload().await?;let state=crate::index::refresh_state(ctx)?;ctx.ui.notify(&format!("Reloaded hooks.\n{}",format_hook_status(&state).map_err(|error|ExtensionFailure::new(error.to_string()))?),NotificationType::Info);return Ok(());}
        let state=crate::index::refresh_state(ctx)?;
        let platform=if cfg!(windows) {"win32"} else {"linux"};
        let records=crate::trust::list_hook_trust_records(&state.parsed.executable_handlers,&state.trust,platform).map_err(|error|ExtensionFailure::new(error.to_string()))?;
        match command {
            HookCommand::List=>ctx.ui.notify(&format_hook_status(&state).map_err(|error|ExtensionFailure::new(error.to_string()))?,NotificationType::Info),
            HookCommand::Diagnostics=>ctx.ui.notify(&format_hook_diagnostics(&state.parsed.diagnostics),if state.parsed.diagnostics.is_empty() {NotificationType::Info} else {NotificationType::Warning}),
            HookCommand::Trust(id)|HookCommand::Disable(id)|HookCommand::Enable(id)=> {
                let Some((handler,record))=state.parsed.executable_handlers.iter().zip(&records).find(|(_,record)|record.id==id) else {ctx.ui.notify(&format!("Hook not found: {id}"),NotificationType::Error);return Ok(());};
                let Some(scope)=crate::trust::hook_trust_storage_scope(handler,ctx.is_project_trusted()) else {ctx.ui.notify(&format!("Hook not found: {id}"),NotificationType::Error);return Ok(());};
                let action=parse_hooks_command(args);let updated_at=chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis,true);
                let entry=if matches!(action,HookCommand::Trust(_)) {crate::trust::create_hook_trust_entry(handler,platform,&updated_at).map_err(|error|ExtensionFailure::new(error.to_string()))?} else {crate::types::HookTrustEntry {enabled:matches!(action,HookCommand::Enable(_)),trusted_hash:record.entry.as_ref().and_then(|entry|entry.trusted_hash.clone()),scope:record.scope.clone(),source_path:record.source_path.clone(),matcher:record.matcher.clone(),command_preview:record.command_preview.clone(),updated_at}};
                state.storage.update(scope,|mut state| {state.version=1;state.hooks.insert(id.clone(),entry);state}).map_err(|error|ExtensionFailure::new(error.to_string()))?;
                let past=match action {HookCommand::Trust(_)=>"Trusted",HookCommand::Disable(_)=>"Disabled",_=>"Enabled"};ctx.ui.notify(&format!("{past} hook: {id}"),NotificationType::Info);
            },_=>{},
        }Ok(())
    })),Arc::new(|prefix| {let prefix=prefix.to_owned();Box::pin(async move {hook_argument_completions(&prefix).map(|items|items.into_iter().map(|(value,label)|maho_ext_api::types::AutocompleteItem {value:value.to_owned(),label:label.to_owned(),description:None}).collect())})}));
}
fn sanitize_display_text(value:&str)->String {
    let mut text=value.to_owned();
    for (pattern,replacement) in [(r#"(?i)(--(?:api-key|apikey|auth|authorization|password|secret|token)=)(?:"[^"]*"|'[^']*'|\S+)"#,"${1}[redacted]"),(r#"(?i)(--(?:api-key|apikey|auth|authorization|password|secret|token)\s+)(?:"[^"]*"|'[^']*'|\S+)"#,"${1}[redacted]"),(r"(?i)\b(Bearer\s+)[A-Za-z0-9._~+/=-]+","${1}[redacted]"),(r"\b(?:sk|pk|rk|glpat)-[A-Za-z0-9._-]+","[redacted]")] {
        text=fancy_regex::Regex::new(pattern).expect("display redaction regex").replace_all(&text,replacement).into_owned();
    }
    crate::output_bounds::redact_hook_token_values(&text,"[redacted]")
}
pub fn format_hook_status(state:&crate::index::HookRuntimeState)->Result<String,crate::trust::TrustError> {
    let mut counts=Vec::new();for handler in &state.parsed.executable_handlers {if let Some((_,count))=counts.iter_mut().find(|(event,_)|*event==handler.event) {*count+=1;} else {counts.push((handler.event,1usize));}}
    let summary=if counts.is_empty() {"hooks: no executable hooks".to_owned()} else {format!("hooks: {} executable hooks ({})",state.parsed.executable_handlers.len(),counts.iter().map(|(event,count)|format!("{}:{count}",event.as_str())).collect::<Vec<_>>().join(", "))};
    let records=crate::trust::list_hook_trust_records(&state.parsed.executable_handlers,&state.trust,if cfg!(windows) {"win32"} else {"linux"})?;
    let mut lines=vec![summary];
    for (handler,record) in state.parsed.executable_handlers.iter().zip(records) {
        let mut parts=vec![format!("- {}",record.id),handler.event.as_str().to_owned(),format!("source:{}",serde_json::to_value(&record.scope).expect("hook scope").as_str().expect("scope string")),format!("status:{}",if record.trusted {"trusted"} else {"untrusted"}),format!("disabled:{}",!record.enabled)];
        if let Some(matcher)=record.matcher {parts.push(format!("matcher:{}",sanitize_display_text(&matcher)));}
        if let Some(message)=&handler.config.status_message {parts.push(format!("statusMessage:{}",sanitize_display_text(message)));}
        parts.push(format!("command:{}",sanitize_display_text(&record.command_preview)));lines.push(parts.join(" "));
    }
    Ok(lines.join("\n"))
}
pub fn format_hook_diagnostics(diagnostics:&[crate::types::HookDiagnostic])->String {
    if diagnostics.is_empty() {return "hooks diagnostics: none".to_owned();}
    let mut lines=vec![format!("hooks diagnostics: {} diagnostics",diagnostics.len())];
    lines.extend(diagnostics.iter().take(50).map(|diagnostic|format!("{}: {} {} {} {}",if diagnostic.severity==crate::types::Severity::Error {"error"} else {"warning"},diagnostic.code,sanitize_display_text(&diagnostic.source.source_path),sanitize_display_text(&diagnostic.path),sanitize_display_text(&diagnostic.message))));
    if diagnostics.len()>50 {lines.push(format!("... {} more diagnostics",diagnostics.len()-50));}
    lines.join("\n")
}
#[derive(Debug,PartialEq,Eq)]
pub enum HookCommand {List,Diagnostics,Reload,Trust(String),Disable(String),Enable(String),Usage}
pub fn parse_hooks_command(args:&str)->HookCommand {let tokens=args.split_whitespace().collect::<Vec<_>>();match tokens.as_slice() {[]|["list"]=>HookCommand::List,["diagnostics"]=>HookCommand::Diagnostics,["reload"]=>HookCommand::Reload,["trust",id]=>HookCommand::Trust((*id).to_owned()),["disable",id]=>HookCommand::Disable((*id).to_owned()),["enable",id]=>HookCommand::Enable((*id).to_owned()),_=>HookCommand::Usage}}
pub fn hook_argument_completions(prefix:&str)->Option<Vec<(&'static str,&'static str)>> {if prefix.chars().any(char::is_whitespace) {return Some(Vec::new());}let completions=HOOK_SUBCOMMANDS.into_iter().filter(|command|command.starts_with(prefix)).map(|command|(command,command)).collect::<Vec<_>>();if completions.is_empty() {None} else {Some(completions)}}
#[cfg(test)]
mod tests {use super::*;#[test] fn exact_argument_arity() {assert_eq!(parse_hooks_command("  "),HookCommand::List);assert_eq!(parse_hooks_command("trust hk_1"),HookCommand::Trust("hk_1".to_owned()));assert_eq!(parse_hooks_command("trust"),HookCommand::Usage);assert_eq!(parse_hooks_command("list extra"),HookCommand::Usage);assert_eq!(parse_hooks_command("reload"),HookCommand::Reload);}#[test] fn prefix_completion_distinguishes_empty_and_absent() {assert_eq!(hook_argument_completions("trust "),Some(Vec::new()));assert_eq!(hook_argument_completions("unknown"),None);assert_eq!(hook_argument_completions("di"),Some(vec![("diagnostics","diagnostics"),("disable","disable")]));}}
#[cfg(test)]
mod redaction_tests {
    use super::*;
    #[test]
    fn command_display_redacts_quoted_arguments_and_bearer_tokens() {
        let output=sanitize_display_text("run --token=\"example secret\" --password 'other secret' Bearer example-token sk-example-token");
        assert!(!output.contains("example"));assert!(!output.contains("other secret"));assert_eq!(output.matches("[redacted]").count(),4);
    }
}
