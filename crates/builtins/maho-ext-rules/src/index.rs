use crate::{
    commands::register_slash_commands,
    config::config_from_environment,
    rules::{cache, engine::Engine, tool_paths::extract_tool_paths, types::RulesMode},
};
use maho_ext_api::{
    BeforeAgentStartEventResult, EventKind, EventResult, Extension, ExtensionApi, ExtensionEvent,
    ExtensionFailure, ExtensionHandlerOptions, ExtensionRuntime, FlagType, FlagValue,
    SessionCompactEvent, ToolContent, ToolResultEventResult,
};
use maho_ext_rule_activation::{
    index::{append_rule_activation, register_rule_activation_renderer},
    types::RuleActivationDetails,
};
use std::{
    collections::BTreeSet,
    path::PathBuf,
    sync::{Arc, Mutex},
};

#[derive(Default)]
pub struct Rules;
impl Extension for Rules {
    fn register(&self, api: &mut ExtensionApi) {
        Self::register_with_config(
            api,
            config_from_environment(|key| std::env::var(key).ok()),
            std::env::var_os("HOME").map_or_else(PathBuf::new, PathBuf::from),
        );
    }
}
impl Rules {
    pub fn register_with_config(
        api: &mut ExtensionApi,
        config: crate::rules::types::PiRulesConfig,
        home_dir: PathBuf,
    ) {
        api.register_flag(
            "pi-rules-disabled",
            FlagType::Boolean {
                default: Some(false),
            },
            Some("Disable pi-rules hooks.".into()),
        );
        api.register_flag(
            "pi-rules-mode",
            FlagType::String {
                default: Some("both".into()),
            },
            Some("Rule injection mode: static, dynamic, both, or off.".into()),
        );
        let env_disabled = config.disabled;
        let engine = Arc::new(Mutex::new(Engine::new(config, home_dir)));
        register_slash_commands(api, Arc::clone(&engine));
        register_rule_activation_renderer(api);
        let actions = Arc::new(ExtensionApi::new(
            api.registered.clone(),
            api.profile.clone(),
            api.events.clone(),
            api.runtime.clone(),
        ));
        for kind in [EventKind::SessionStart, EventKind::SessionCompact] {
            let engine = Arc::clone(&engine);
            let actions = Arc::clone(&actions);
            let runtime = api.runtime.clone();
            api.on(
                kind,
                Arc::new(move |event, ctx| {
                    let engine = Arc::clone(&engine);
                    let actions = Arc::clone(&actions);
                    let runtime = runtime.clone();
                    Box::pin(async move {
                        let mut engine = engine
                            .lock()
                            .map_err(|error| ExtensionFailure::new(error.to_string()))?;
                        let reason = if kind == EventKind::SessionStart {
                            sync_config(&mut engine, &runtime, env_disabled);
                            if engine.config.disabled {
                                return Ok(EventResult::None);
                            }
                            let ExtensionEvent::SessionStart(event) = event else {
                                return Ok(EventResult::None);
                            };
                            serde_json::Value::String(
                                match event.reason {
                                    maho_ext_api::SessionReason::Startup => "startup",
                                    maho_ext_api::SessionReason::Reload => "reload",
                                    maho_ext_api::SessionReason::New => "new",
                                    maho_ext_api::SessionReason::Resume => "resume",
                                    maho_ext_api::SessionReason::Fork => "fork",
                                    maho_ext_api::SessionReason::Quit => "quit",
                                }
                                .into(),
                            )
                        } else {
                            if matches!(
                                event,
                                ExtensionEvent::SessionCompact(
                                    SessionCompactEvent::Rejected { .. }
                                )
                            ) {
                                return Ok(EventResult::None);
                            }
                            serde_json::Value::String("compact".into())
                        };
                        engine.reset_session(Some(&ctx.cwd.to_string_lossy()));
                        actions.append_entry(
                            "pi-rules.scan",
                            Some(serde_json::json!({"cwd":ctx.cwd,"reason":reason})),
                        )?;
                        Ok(EventResult::None)
                    })
                }),
            );
        }
        // Absolute paths (and realpaths) of context files loaded natively into the system prompt, rebuilt on every
        // committed before_agent_start; the dynamic tool_result path must not re-inject them either.
        let native_context: Arc<Mutex<BTreeSet<String>>> = Arc::default();
        let shared = Arc::clone(&engine);
        let runtime = api.runtime.clone();
        let shared_native = Arc::clone(&native_context);
        api.on_with_options(
            EventKind::BeforeAgentStart,
            Arc::new(move |event, ctx| {
                let engine = Arc::clone(&shared);
                let runtime = runtime.clone();
                let native_context = Arc::clone(&shared_native);
                Box::pin(async move {
                    let ExtensionEvent::BeforeAgentStart(event) = event else {
                        return Ok(EventResult::None);
                    };
                    let mut engine = engine
                        .lock()
                        .map_err(|error| ExtensionFailure::new(error.to_string()))?;
                    sync_config(&mut engine, &runtime, env_disabled);
                    if engine.config.disabled
                        || matches!(engine.config.mode, RulesMode::Off | RulesMode::Dynamic)
                    {
                        return Ok(EventResult::None);
                    }
                    let loaded = engine.load_static_rules(&ctx.cwd);
                    let native: BTreeSet<_> = event
                        .system_prompt_options
                        .context_files
                        .iter()
                        .flat_map(|file| {
                            let mut keys = vec![file.path.clone()];
                            if let Ok(path) = std::fs::canonicalize(&file.path) {
                                keys.push(path.to_string_lossy().into_owned());
                            }
                            keys
                        })
                        .collect();
                    // A preview composes the same block but leaves the injection marks untouched: no turn follows it.
                    let commit = !event.preview;
                    if commit {
                        *native_context
                            .lock()
                            .map_err(|error| ExtensionFailure::new(error.to_string()))? =
                            native.clone();
                    }
                    for rule in &loaded.rules {
                        if commit
                            && (native.contains(&rule.candidate.path)
                                || native.contains(&rule.candidate.real_path))
                        {
                            cache::mark_static_injected(&mut engine.state, rule);
                        }
                    }
                    let rules: Vec<_> = loaded
                        .rules
                        .into_iter()
                        .filter(|rule| {
                            !native.contains(&rule.candidate.path)
                                && !native.contains(&rule.candidate.real_path)
                        })
                        .collect();
                    if rules.is_empty() {
                        return Ok(EventResult::None);
                    }
                    let block = engine.format_static(&rules);
                    if commit {
                        for rule in &rules {
                            cache::mark_static_injected(&mut engine.state, rule);
                        }
                    }
                    Ok(EventResult::BeforeAgentStart(BeforeAgentStartEventResult {
                        system_prompt: Some(format!("{}{block}", event.system_prompt)),
                        ..Default::default()
                    }))
                })
            }),
            ExtensionHandlerOptions {
                preview_safe: true,
                ..Default::default()
            },
        );
        let runtime = api.runtime.clone();
        api.on(
            EventKind::ToolResult,
            Arc::new(move |event, ctx| {
                let engine = Arc::clone(&engine);
                let runtime = runtime.clone();
                let actions = Arc::clone(&actions);
                let native_context = Arc::clone(&native_context);
                Box::pin(async move {
                    let ExtensionEvent::ToolResult(event) = event else {
                        return Ok(EventResult::None);
                    };
                    let mut engine = engine
                        .lock()
                        .map_err(|error| ExtensionFailure::new(error.to_string()))?;
                    sync_config(&mut engine, &runtime, env_disabled);
                    if engine.config.disabled
                        || matches!(engine.config.mode, RulesMode::Off | RulesMode::Static)
                        || event.is_error
                    {
                        return Ok(EventResult::None);
                    }
                    let targets = extract_tool_paths(event, &ctx.cwd);
                    if targets.is_empty() {
                        return Ok(EventResult::None);
                    }
                    let fingerprints = engine.fingerprint_dynamic_targets(&ctx.cwd, &targets);
                    let pending: Vec<_> = fingerprints
                        .iter()
                        .filter(|target| !engine.is_dynamic_target_fingerprint_current(target))
                        .map(|target| target.target_path.clone())
                        .collect();
                    if pending.is_empty() {
                        engine.commit_dynamic_target_fingerprints(&fingerprints);
                        return Ok(EventResult::None);
                    }
                    let loaded = engine
                        .load_dynamic_rules(&ctx.cwd, &pending)
                        .map_err(|error| ExtensionFailure::new(error.to_string()))?;
                    engine.commit_dynamic_target_fingerprints(&fingerprints);
                    let native = native_context
                        .lock()
                        .map_err(|error| ExtensionFailure::new(error.to_string()))?
                        .clone();
                    let is_native = |rule: &crate::rules::types::LoadedRule| {
                        native.contains(&rule.candidate.path)
                            || native.contains(&rule.candidate.real_path)
                    };
                    for rule in &loaded.rules {
                        if is_native(rule) {
                            cache::mark_static_injected(&mut engine.state, rule);
                        }
                    }
                    let rules: Vec<_> = loaded
                        .rules
                        .into_iter()
                        .filter(|rule| {
                            !is_native(rule)
                                && !cache::is_static_injected(&engine.state, rule)
                                && !cache::is_dynamic_injected(&engine.state, "live-context", rule)
                        })
                        .collect();
                    if rules.is_empty() {
                        return Ok(EventResult::None);
                    }
                    let target = display_path(&ctx.cwd, pending.first().unwrap_or(&targets[0]));
                    let block = engine.format_dynamic(&rules, &target);
                    for rule in &rules {
                        cache::mark_dynamic_injected(&mut engine.state, "live-context", rule);
                    }
                    append_rule_activation(
                        &actions,
                        &RuleActivationDetails::ProjectRules {
                            target_path: target,
                            rules: rules
                                .iter()
                                .map(|rule| rule.candidate.relative_path.clone())
                                .collect(),
                            tool_call_id: Some(event.tool_call_id.clone()),
                        },
                    )?;
                    let mut content = event.content.clone();
                    content.push(ToolContent::Text {
                        text: block,
                        audience: Some("model".into()),
                    });
                    Ok(EventResult::ToolResult(ToolResultEventResult {
                        content: Some(content),
                        ..Default::default()
                    }))
                })
            }),
        );
    }
}
fn sync_config(engine: &mut Engine, runtime: &ExtensionRuntime, env_disabled: bool) {
    if let Some(FlagValue::Boolean(disabled)) = runtime.get_flag("pi-rules-disabled") {
        engine.config.disabled = disabled || env_disabled;
    }
    if let Some(FlagValue::String(mode)) = runtime.get_flag("pi-rules-mode")
        && let Some(mode) = match mode.as_str() {
            "static" => Some(RulesMode::Static),
            "dynamic" => Some(RulesMode::Dynamic),
            "both" => Some(RulesMode::Both),
            "off" => Some(RulesMode::Off),
            _ => None,
        }
    {
        engine.config.mode = mode;
    }
}
/// `isAbsolute(filePath) ? relative(cwd, filePath) : filePath`.
fn display_path(cwd: &std::path::Path, file_path: &std::path::Path) -> String {
    if file_path.is_absolute() {
        crate::rules::engine::relative_path(cwd, file_path)
    } else {
        file_path.to_string_lossy().into_owned()
    }
}
