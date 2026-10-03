use std::collections::BTreeSet;
use std::sync::{Arc,Mutex};
use maho_ext_api::{ExtensionApi,NotificationType};
use crate::rules::{engine::{Engine,EngineDeps},types::{LoadedRule,MatchReason,RuleDiagnostic,Severity}};
pub fn register_slash_commands<D:EngineDeps+Send+'static>(api:&mut ExtensionApi,engine:Arc<Mutex<Engine<D>>>){
    let rules=Arc::clone(&engine);
    api.register_command_with_completions("rules",Some("Inspect loaded pi-rules.".into()),None,Arc::new(move|args,ctx|{
        let (message,severity)={let mut engine=rules.lock().unwrap_or_else(std::sync::PoisonError::into_inner);handle_rules(&mut engine,args,&ctx.cwd.to_string_lossy())};
        ctx.ui.notify(&message,match severity{Some(Severity::Error)=>NotificationType::Error,Some(Severity::Warning)=>NotificationType::Warning,None=>NotificationType::Info});
        Box::pin(async{Ok(())})
    }), Arc::new(|prefix| Box::pin(async move {
        Ok(argument_completions(prefix).map(|items| items.into_iter().map(|(value, label)|
            maho_tui::autocomplete::AutocompleteItem { value: value.into(), label: label.into(), description: None }
        ).collect()))
    })));
    api.register_command("reload-rules",Some("Reload pi-rules for the current session.".into()),None,Arc::new(move|_,ctx|{
        let message={let mut engine=engine.lock().unwrap_or_else(std::sync::PoisonError::into_inner);handle_reload(&mut engine,&ctx.cwd.to_string_lossy())};
        ctx.ui.notify(&message,NotificationType::Info);Box::pin(async{Ok(())})
    }));
}
pub const RULE_SUBCOMMANDS:[&str;4]=["list","show","paths","status"];
pub fn argument_completions(prefix:&str)->Option<Vec<(&'static str,&'static str)>>{let result:Vec<_>=RULE_SUBCOMMANDS.into_iter().filter(|command|command.starts_with(prefix)).map(|command|(command,command)).collect();(!result.is_empty()).then_some(result)}
pub fn handle_rules<D:EngineDeps>(engine:&mut Engine<D>,args:&str,cwd:&str)->(String,Option<Severity>){
    let tokens:Vec<_>=args.split(|ch|matches!(ch,'\u{0009}'..='\u{000d}'|'\u{0020}'|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}')).filter(|token|!token.is_empty()).collect();let subcommand=tokens.first().copied().unwrap_or("");let loaded=engine.load_static_rules(cwd);
    match subcommand{
        ""|"status"=>(build_summary_text(&loaded.rules,&loaded.diagnostics),None),
        "list"=>(format_rule_list(&loaded.rules),None),
        "show"=>{let Some(id)=tokens.get(1)else{return ("Rule ID is required".into(),Some(Severity::Error));};match find_rule_by_id(&loaded.rules,id){Some(rule)=>(rule.body.clone(),None),None=>(format!("Rule not found: {id}"),Some(Severity::Error))}},
        "paths"=>(loaded.rules.iter().map(|rule|rule.candidate.path.as_str()).collect::<Vec<_>>().join("\n"),None),
        _=>(format!("Unknown /rules subcommand: {subcommand}"),Some(Severity::Error)),
    }
}
pub fn handle_reload<D:EngineDeps>(engine:&mut Engine<D>,cwd:&str)->String{engine.reset_session(Some(cwd));let loaded=engine.load_static_rules(cwd);append_diagnostics(format!("Reloaded {} rules from {} sources",loaded.rules.len(),count_sources(&loaded.rules)),&loaded.diagnostics)}
pub fn build_summary_text(rules:&[LoadedRule],diagnostics:&[RuleDiagnostic])->String{append_diagnostics(format!("pi-rules: {} rules from {} sources",rules.len(),count_sources(rules)),diagnostics)}
fn append_diagnostics(text:String,diagnostics:&[RuleDiagnostic])->String{if diagnostics.is_empty(){text}else{format!("{text}, {} diagnostics",diagnostics.len())}}
fn count_sources(rules:&[LoadedRule])->usize{rules.iter().map(|rule|&rule.candidate.source).collect::<BTreeSet<_>>().len()}
fn format_rule_list(rules:&[LoadedRule])->String{rules.iter().map(|rule|format!("{} [{}, {}]",rule.candidate.relative_path,rule.candidate.source,format_match_reason(&rule.match_reason))).collect::<Vec<_>>().join("\n")}
fn format_match_reason(reason:&MatchReason)->String{match reason{MatchReason::AlwaysApply=>"alwaysApply".into(),MatchReason::SingleFile=>"single-file".into(),MatchReason::NoMatch=>"no-match".into(),MatchReason::Glob{pattern}=>format!("glob:{pattern}")}}
fn find_rule_by_id<'a>(rules:&'a [LoadedRule],id:&str)->Option<&'a LoadedRule>{if let Some(rule)=rules.iter().find(|rule|rule.candidate.relative_path==id){return Some(rule);}let mut matches=rules.iter().filter(|rule|rule.candidate.relative_path.ends_with(id));let result=matches.next();if matches.next().is_some(){None}else{result}}
