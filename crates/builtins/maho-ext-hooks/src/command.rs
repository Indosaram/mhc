pub const HOOKS_USAGE:&str="Usage: /hooks [list|diagnostics|trust <id>|disable <id>|enable <id>|reload]";
pub const HOOK_SUBCOMMANDS:[&str;6]=["list","diagnostics","trust","disable","enable","reload"];
pub fn register_hooks_command(api:&mut maho_ext_api::types::ExtensionApi) {
    use maho_ext_api::types::{ExtensionFailure,NotificationType};use std::sync::Arc;
    api.register_command_with_context("hooks",Some("Inspect loaded builtin hook sources and diagnostics.".to_owned()),None,Arc::new(|args,ctx|Box::pin(async move {
        let command=parse_hooks_command(args);
        if command==HookCommand::Usage {ctx.ui.notify(HOOKS_USAGE,NotificationType::Error);return Ok(());}
        if command==HookCommand::Reload {ctx.reload().await?;ctx.ui.notify("Reloaded hooks.",NotificationType::Info);return Ok(());}
        let state=crate::index::refresh_state(ctx)?;
        let platform=if cfg!(windows) {"win32"} else {"linux"};
        let records=crate::trust::list_hook_trust_records(&state.parsed.executable_handlers,&state.trust,platform).map_err(|error|ExtensionFailure::new(error.to_string()))?;
        match command {
            HookCommand::List=> {let mut counts=Vec::<(crate::types::SupportedHookEvent,usize)>::new();for handler in &state.parsed.executable_handlers {if let Some((_,count))=counts.iter_mut().find(|(event,_)|*event==handler.event) {*count+=1;} else {counts.push((handler.event,1));}}
                let summary=if counts.is_empty() {"hooks: no executable hooks".to_owned()} else {format!("hooks: {} executable hooks ({})",records.len(),counts.iter().map(|(event,count)|format!("{}:{count}",event.as_str())).collect::<Vec<_>>().join(", "))};ctx.ui.notify(&summary,NotificationType::Info);
            },
            HookCommand::Diagnostics=> {let diagnostics=&state.parsed.diagnostics;let text=if diagnostics.is_empty() {"hooks diagnostics: none".to_owned()} else {format!("hooks diagnostics: {} diagnostics",diagnostics.len())};ctx.ui.notify(&text,if diagnostics.is_empty() {NotificationType::Info} else {NotificationType::Warning});},
            HookCommand::Trust(id)|HookCommand::Disable(id)|HookCommand::Enable(id)=> {
                let Some((handler,record))=state.parsed.executable_handlers.iter().zip(&records).find(|(_,record)|record.id==id) else {ctx.ui.notify(&format!("Hook not found: {id}"),NotificationType::Error);return Ok(());};
                let Some(scope)=crate::trust::hook_trust_storage_scope(handler,ctx.is_project_trusted()) else {ctx.ui.notify(&format!("Hook not found: {id}"),NotificationType::Error);return Ok(());};
                let action=parse_hooks_command(args);let updated_at=chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis,true);
                let entry=if matches!(action,HookCommand::Trust(_)) {crate::trust::create_hook_trust_entry(handler,platform,&updated_at).map_err(|error|ExtensionFailure::new(error.to_string()))?} else {crate::types::HookTrustEntry {enabled:matches!(action,HookCommand::Enable(_)),trusted_hash:record.entry.as_ref().and_then(|entry|entry.trusted_hash.clone()),scope:record.scope.clone(),source_path:record.source_path.clone(),matcher:record.matcher.clone(),command_preview:record.command_preview.clone(),updated_at}};
                state.storage.update(scope,|mut state| {state.version=1;state.hooks.insert(id.clone(),entry);state}).map_err(|error|ExtensionFailure::new(error.to_string()))?;
                let past=match action {HookCommand::Trust(_)=>"Trusted",HookCommand::Disable(_)=>"Disabled",_=>"Enabled"};ctx.ui.notify(&format!("{past} hook: {id}"),NotificationType::Info);
            },_=>{},
        }Ok(())
    })));
}
#[derive(Debug,PartialEq,Eq)]
pub enum HookCommand {List,Diagnostics,Reload,Trust(String),Disable(String),Enable(String),Usage}
pub fn parse_hooks_command(args:&str)->HookCommand {let tokens=args.split_whitespace().collect::<Vec<_>>();match tokens.as_slice() {[]|["list"]=>HookCommand::List,["diagnostics"]=>HookCommand::Diagnostics,["reload"]=>HookCommand::Reload,["trust",id]=>HookCommand::Trust((*id).to_owned()),["disable",id]=>HookCommand::Disable((*id).to_owned()),["enable",id]=>HookCommand::Enable((*id).to_owned()),_=>HookCommand::Usage}}
pub fn hook_argument_completions(prefix:&str)->Option<Vec<(&'static str,&'static str)>> {if prefix.chars().any(char::is_whitespace) {return Some(Vec::new());}let completions=HOOK_SUBCOMMANDS.into_iter().filter(|command|command.starts_with(prefix)).map(|command|(command,command)).collect::<Vec<_>>();if completions.is_empty() {None} else {Some(completions)}}
#[cfg(test)]
mod tests {use super::*;#[test] fn exact_argument_arity() {assert_eq!(parse_hooks_command("  "),HookCommand::List);assert_eq!(parse_hooks_command("trust hk_1"),HookCommand::Trust("hk_1".to_owned()));assert_eq!(parse_hooks_command("trust"),HookCommand::Usage);assert_eq!(parse_hooks_command("list extra"),HookCommand::Usage);assert_eq!(parse_hooks_command("reload"),HookCommand::Reload);}#[test] fn prefix_completion_distinguishes_empty_and_absent() {assert_eq!(hook_argument_completions("trust "),Some(Vec::new()));assert_eq!(hook_argument_completions("unknown"),None);assert_eq!(hook_argument_completions("di"),Some(vec![("diagnostics","diagnostics"),("disable","disable")]));}}
