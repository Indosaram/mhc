use std::{collections::BTreeSet, sync::{Arc, Mutex}};
use maho_ext_api::{ExtensionApi, ExtensionFailure, NotificationType};
use crate::rules::{engine::Engine, types::{LoadedRule, MatchReason}};

pub fn register_slash_commands(api: &mut ExtensionApi, engine: Arc<Mutex<Engine>>) {
    let shared = Arc::clone(&engine);
    api.register_command("rules", Some("Inspect loaded pi-rules.".into()), None, Arc::new(move |args, ctx| {
        let shared = Arc::clone(&shared);
        Box::pin(async move {
            let mut engine = shared.lock().map_err(|error| ExtensionFailure::new(error.to_string()))?;
            let loaded = engine.load_static_rules(&ctx.cwd);
            let tokens: Vec<_> = args.split_whitespace().collect();
            let command = tokens.first().copied().unwrap_or("");
            let (message, severity) = match command {
                "" | "status" => (summary(&loaded.rules, loaded.diagnostics.len(), false), NotificationType::Info),
                "list" => (loaded.rules.iter().map(|rule| format!("{} [{}, {}]", rule.candidate.relative_path, rule.candidate.source, match_reason(&rule.match_reason))).collect::<Vec<_>>().join("\n"), NotificationType::Info),
                "paths" => (loaded.rules.iter().map(|rule| rule.candidate.path.as_str()).collect::<Vec<_>>().join("\n"), NotificationType::Info),
                "show" => {
                    let id = tokens.get(1).copied().unwrap_or("");
                    match find_rule_by_id(&loaded.rules, id) {
                        Some(rule) => (rule.body.clone(), NotificationType::Info),
                        None => (format!("Rule not found: {id}"), NotificationType::Error),
                    }
                },
                other => (format!("Unknown /rules subcommand: {other}"), NotificationType::Error),
            };
            ctx.ui.notify(&message, severity);
            Ok(())
        })
    }));
    api.register_command("reload-rules", Some("Reload pi-rules for the current session.".into()), None, Arc::new(move |_, ctx| {
        let shared = Arc::clone(&engine);
        Box::pin(async move {
            let mut engine = shared.lock().map_err(|error| ExtensionFailure::new(error.to_string()))?;
            engine.reset_session(Some(&ctx.cwd.to_string_lossy()));
            let loaded = engine.load_static_rules(&ctx.cwd);
            ctx.ui.notify(&summary(&loaded.rules, loaded.diagnostics.len(), true), NotificationType::Info);
            Ok(())
        })
    }));
}
fn summary(rules: &[LoadedRule], diagnostics: usize, reloaded: bool) -> String {
    let sources: BTreeSet<_> = rules.iter().map(|rule| &rule.candidate.source).collect();
    let text = if reloaded { format!("Reloaded {} rules from {} sources", rules.len(), sources.len()) } else { format!("pi-rules: {} rules from {} sources", rules.len(), sources.len()) };
    if diagnostics == 0 { text } else { format!("{text}, {diagnostics} diagnostics") }
}
fn match_reason(reason: &MatchReason) -> String {
    match reason { MatchReason::AlwaysApply => "alwaysApply".into(), MatchReason::SingleFile => "single-file".into(), MatchReason::NoMatch => "no-match".into(), MatchReason::Glob { pattern } => format!("glob:{pattern}") }
}
pub fn find_rule_by_id<'a>(rules: &'a [LoadedRule], id: &str) -> Option<&'a LoadedRule> {
    if let Some(rule) = rules.iter().find(|rule| rule.candidate.relative_path == id) { return Some(rule); }
    let mut matches = rules.iter().filter(|rule| rule.candidate.relative_path.ends_with(id));
    let found = matches.next()?;
    if matches.next().is_some() { None } else { Some(found) }
}
