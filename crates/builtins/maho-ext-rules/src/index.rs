use std::{collections::BTreeSet, path::PathBuf, sync::{Arc, Mutex}};
use maho_ext_api::{BeforeAgentStartEventResult, EventKind, EventResult, Extension, ExtensionApi, ExtensionEvent, ExtensionFailure, ExtensionRuntime, FlagType, FlagValue, SessionCompactEvent, ToolContent, ToolResultEventResult};
use maho_ext_rule_activation::{index::{append_rule_activation, register_rule_activation_renderer}, types::RuleActivationDetails};
use crate::{commands::register_slash_commands, config::config_from_environment, rules::{cache, engine::Engine, tool_paths::extract_tool_paths, types::RulesMode}};

#[derive(Default)]
pub struct Rules;
impl Extension for Rules {
    fn register(&self, api: &mut ExtensionApi) {
        Self::register_with_config(api, config_from_environment(|key| std::env::var(key).ok()), std::env::var_os("HOME").map_or_else(PathBuf::new, PathBuf::from));
    }
}
impl Rules {
    pub fn register_with_config(api: &mut ExtensionApi, config: crate::rules::types::PiRulesConfig, home_dir: PathBuf) {
        api.register_flag("pi-rules-disabled", FlagType::Boolean { default: Some(false) }, Some("Disable pi-rules hooks.".into()));
        api.register_flag("pi-rules-mode", FlagType::String { default: Some("both".into()) }, Some("Rule injection mode: static, dynamic, both, or off.".into()));
        let env_disabled = config.disabled;
        let engine = Arc::new(Mutex::new(Engine::new(config, home_dir)));
        register_slash_commands(api, Arc::clone(&engine));
        register_rule_activation_renderer(api);
        let actions = Arc::new(ExtensionApi::new(api.registered.clone(), api.profile.clone(), api.events.clone(), api.runtime.clone()));
        for kind in [EventKind::SessionStart, EventKind::SessionCompact] {
            let engine = Arc::clone(&engine); let actions = Arc::clone(&actions); let runtime = api.runtime.clone();
            api.on(kind, Arc::new(move |event, ctx| {
                let engine = Arc::clone(&engine); let actions = Arc::clone(&actions); let runtime = runtime.clone();
                Box::pin(async move {
                    let mut engine = engine.lock().map_err(|error| ExtensionFailure::new(error.to_string()))?;
                    let reason = if kind == EventKind::SessionStart {
                        sync_config(&mut engine, &runtime, env_disabled);
                        if engine.config.disabled { return Ok(EventResult::None); }
                        let ExtensionEvent::SessionStart(event) = event else { return Ok(EventResult::None); };
                        serde_json::Value::String(match event.reason {
                            maho_ext_api::SessionReason::Startup => "startup", maho_ext_api::SessionReason::Reload => "reload", maho_ext_api::SessionReason::New => "new", maho_ext_api::SessionReason::Resume => "resume", maho_ext_api::SessionReason::Fork => "fork", maho_ext_api::SessionReason::Quit => "quit",
                        }.into())
                    } else {
                        if matches!(event, ExtensionEvent::SessionCompact(SessionCompactEvent::Rejected { .. })) { return Ok(EventResult::None); }
                        serde_json::Value::String("compact".into())
                    };
                    engine.reset_session(Some(&ctx.cwd.to_string_lossy()));
                    actions.append_entry("pi-rules.scan", Some(serde_json::json!({"cwd":ctx.cwd,"reason":reason})))?;
                    Ok(EventResult::None)
                })
            }));
        }
        let shared = Arc::clone(&engine); let runtime = api.runtime.clone();
        api.on(EventKind::BeforeAgentStart, Arc::new(move |event, ctx| {
            let engine = Arc::clone(&shared); let runtime = runtime.clone();
            Box::pin(async move {
                let ExtensionEvent::BeforeAgentStart(event) = event else { return Ok(EventResult::None); };
                let mut engine = engine.lock().map_err(|error| ExtensionFailure::new(error.to_string()))?;
                sync_config(&mut engine, &runtime, env_disabled);
                if engine.config.disabled || matches!(engine.config.mode, RulesMode::Off | RulesMode::Dynamic) { return Ok(EventResult::None); }
                let loaded = engine.load_static_rules(&ctx.cwd);
                let native: BTreeSet<_> = event.system_prompt_options.context_files.iter().flat_map(|file| {
                    let mut keys = vec![file.path.clone()];
                    if let Ok(path) = std::fs::canonicalize(&file.path) { keys.push(path.to_string_lossy().into_owned()); }
                    keys
                }).collect();
                for rule in &loaded.rules { if native.contains(&rule.candidate.path) || native.contains(&rule.candidate.real_path) { cache::mark_static_injected(&mut engine.state, rule); } }
                let rules: Vec<_> = loaded.rules.into_iter().filter(|rule| !native.contains(&rule.candidate.path) && !native.contains(&rule.candidate.real_path)).collect();
                if rules.is_empty() { return Ok(EventResult::None); }
                let block = engine.format_static(&rules);
                for rule in &rules { cache::mark_static_injected(&mut engine.state, rule); }
                Ok(EventResult::BeforeAgentStart(BeforeAgentStartEventResult { system_prompt: Some(format!("{}{block}", event.system_prompt)), ..Default::default() }))
            })
        }));
        let runtime = api.runtime.clone();
        api.on(EventKind::ToolResult, Arc::new(move |event, ctx| {
            let engine = Arc::clone(&engine); let runtime = runtime.clone(); let actions = Arc::clone(&actions);
            Box::pin(async move {
                let ExtensionEvent::ToolResult(event) = event else { return Ok(EventResult::None); };
                let mut engine = engine.lock().map_err(|error| ExtensionFailure::new(error.to_string()))?;
                sync_config(&mut engine, &runtime, env_disabled);
                if engine.config.disabled || matches!(engine.config.mode, RulesMode::Off | RulesMode::Static) || event.is_error { return Ok(EventResult::None); }
                let targets = extract_tool_paths(event, &ctx.cwd);
                if targets.is_empty() { return Ok(EventResult::None); }
                let fingerprints = engine.fingerprint_dynamic_targets(&ctx.cwd, &targets);
                let pending: Vec<_> = fingerprints.iter().filter(|target| !engine.is_dynamic_target_fingerprint_current(target)).map(|target| target.target_path.clone()).collect();
                if pending.is_empty() { engine.commit_dynamic_target_fingerprints(&fingerprints); return Ok(EventResult::None); }
                let loaded = engine.load_dynamic_rules(&ctx.cwd, &pending).map_err(|error| ExtensionFailure::new(error.to_string()))?;
                engine.commit_dynamic_target_fingerprints(&fingerprints);
                let rules: Vec<_> = loaded.rules.into_iter().filter(|rule| !cache::is_static_injected(&engine.state, rule) && !cache::is_dynamic_injected(&engine.state, "live-context", rule)).collect();
                if rules.is_empty() { return Ok(EventResult::None); }
                let target = pending[0].strip_prefix(&ctx.cwd).unwrap_or(&pending[0]).to_string_lossy().into_owned();
                let block = engine.format_dynamic(&rules, &target);
                for rule in &rules { cache::mark_dynamic_injected(&mut engine.state, "live-context", rule); }
                append_rule_activation(&actions, &RuleActivationDetails::ProjectRules { target_path: target, rules: rules.iter().map(|rule| rule.candidate.relative_path.clone()).collect(), tool_call_id: Some(event.tool_call_id.clone()) })?;
                let mut content = event.content.clone();
                content.push(ToolContent::Text { text: block, audience: Some("model".into()) });
                Ok(EventResult::ToolResult(ToolResultEventResult { content: Some(content), ..Default::default() }))
            })
        }));
    }
}
fn sync_config(engine: &mut Engine, runtime: &ExtensionRuntime, env_disabled: bool) {
    if let Some(FlagValue::Boolean(disabled)) = runtime.get_flag("pi-rules-disabled") { engine.config.disabled = disabled || env_disabled; }
    if let Some(FlagValue::String(mode)) = runtime.get_flag("pi-rules-mode")
        && let Some(mode) = match mode.as_str() { "static" => Some(RulesMode::Static), "dynamic" => Some(RulesMode::Dynamic), "both" => Some(RulesMode::Both), "off" => Some(RulesMode::Off), _ => None } { engine.config.mode = mode; }
}
