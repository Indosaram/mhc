//! The session-start model-profile extension (spec model-selection §6.7–6.8).

use crate::probe::{
    Account, AuthProbe, ProbeError, ProbeFailure, ProbeReason, probe_and_resolve,
    rotation_allowed_by_models_json,
};
use crate::profile::{
    ModelProfile, ProfileResolution, RETIRED_PROFILE_ID, effective_profile, merge_profiles,
    resolve_default, resolve_profile,
};
use maho_ai::types::ModelThinkingLevel;
use maho_ext_api::{
    CustomMessage, EventKind, EventResult, Extension, ExtensionApi, ExtensionEvent,
    ExtensionFailure, ExtensionMode, ModelRegistry, NotificationType, SendMessageOptions,
    SessionReason,
};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

/// Custom message type ids (neutral names).
pub const APPLIED_TYPE: &str = "model-profile:applied";
pub const UNAVAILABLE_TYPE: &str = "model-profile:unavailable";
pub const UNKNOWN_TYPE: &str = "model-profile:unknown";

/// Whether the profile applies to this session start (§6.7, without the once-per-session check).
pub fn should_apply(reason: SessionReason, provenance: Option<&str>, mode: ExtensionMode) -> bool {
    matches!(reason, SessionReason::Startup | SessionReason::New)
        && provenance.is_some_and(|p| p != "cli" && p != "scoped")
        && mode != ExtensionMode::Tui
}

/// One apply per session id (one per instance when no id is available).
#[derive(Default)]
pub struct AppliedSessions {
    seen: Mutex<HashSet<String>>,
}

impl AppliedSessions {
    /// `true` the first time `session_id` is seen.
    pub fn first(&self, session_id: &str) -> bool {
        self.seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(session_id.to_owned())
    }
}

/// The session thinking level for a rung reasoning (`auto`/unknown → leave unchanged).
pub fn thinking_level_for(reasoning: Option<&str>) -> Option<ModelThinkingLevel> {
    Some(match reasoning?.trim().to_ascii_lowercase().as_str() {
        "off" | "none" => ModelThinkingLevel::Off,
        "minimal" => ModelThinkingLevel::Minimal,
        "low" => ModelThinkingLevel::Low,
        "medium" => ModelThinkingLevel::Medium,
        "high" => ModelThinkingLevel::High,
        "xhigh" => ModelThinkingLevel::Xhigh,
        "max" => ModelThinkingLevel::Max,
        _ => return None,
    })
}

fn advice(failure: &ProbeFailure, rpc: bool) -> String {
    match failure.reason {
        ProbeReason::Refresh if rpc => format!(
            "re-authenticate {} in the provider settings",
            failure.provider
        ),
        ProbeReason::Refresh => format!("log in to {} again", failure.provider),
        ProbeReason::Credentials => {
            format!("check the credential configuration of {}", failure.provider)
        }
        ProbeReason::Request => format!(
            "check the headers of {}/{} in models.json",
            failure.provider, failure.model
        ),
    }
}

fn failures_json(failures: &[ProbeFailure]) -> Value {
    Value::Array(
        failures
            .iter()
            .map(|f| json!({"provider": f.provider, "model": f.model, "reason": f.reason.as_str()}))
            .collect(),
    )
}

/// The notice for a resolution: `(custom type, text, details)`.
pub fn notice_for(
    result: &ProfileResolution,
    profiles: &[ModelProfile],
    failures: &[ProbeFailure],
    rpc: bool,
) -> (&'static str, String, Value) {
    let label = |id: &str| {
        profiles
            .iter()
            .find(|p| p.id == id && p.display_name != id)
            .map_or_else(|| id.to_owned(), |p| format!("{id} ({})", p.display_name))
    };
    let failure_lines: Vec<String> = failures
        .iter()
        .map(|f| {
            format!(
                "{}/{} skipped ({}): {}",
                f.provider,
                f.model,
                f.reason.as_str(),
                advice(f, rpc)
            )
        })
        .collect();
    let tail = if failure_lines.is_empty() {
        String::new()
    } else {
        format!(
            " {}. Failures later in the session are handled by the retry chains in settings.",
            failure_lines.join("; ")
        )
    };
    match result {
        ProfileResolution::Resolved {
            profile,
            provider,
            model_id,
            reasoning,
            skipped,
            ..
        } => {
            let mut text = format!(
                "mhc: model profile {} selected {provider}/{model_id}",
                label(profile)
            );
            if let Some(reasoning) = reasoning {
                text.push_str(&format!(" at {reasoning} reasoning"));
            }
            if !skipped.is_empty() {
                text.push_str(&format!(" (skipped {})", skipped.join(", ")));
            }
            text.push_str(&tail);
            (
                APPLIED_TYPE,
                text,
                json!({"profile": profile, "model": format!("{provider}/{model_id}"), "reasoning": reasoning,
                       "skipped": skipped, "authFailures": failures_json(failures)}),
            )
        }
        ProfileResolution::Unavailable { profile, chain } => (
            UNAVAILABLE_TYPE,
            format!(
                "mhc: no model of profile {} is available (tried {}); keeping the current model.{tail}",
                label(profile),
                chain.join(", ")
            ),
            json!({"profile": profile, "chain": chain, "authFailures": failures_json(failures)}),
        ),
        ProfileResolution::Empty { profile } => (
            UNAVAILABLE_TYPE,
            format!(
                "mhc: model profile {} has no models; keeping the current model.",
                label(profile)
            ),
            json!({"profile": profile, "chain": []}),
        ),
        ProfileResolution::Unknown {
            message,
            name,
            known,
        } => (
            UNKNOWN_TYPE,
            format!("mhc: {message}; keeping the current model."),
            json!({"profile": name, "known": known}),
        ),
    }
}

/// Probe backed by the session's model registry.
pub struct RegistryProbe {
    pub registry: Arc<dyn ModelRegistry>,
    pub agent_dir: PathBuf,
}

fn probe_error(failure: ExtensionFailure) -> ProbeError {
    ProbeError {
        class: failure.class,
        code: failure.code,
    }
}

impl AuthProbe for RegistryProbe {
    fn rotation_allowed(&self, provider: &str) -> impl std::future::Future<Output = bool> + Send {
        let runtime_key = self
            .registry
            .get_provider_auth_status(provider)
            .source
            .is_some_and(|source| source.contains("runtime"));
        let allowed = !runtime_key && rotation_allowed_by_models_json(&self.agent_dir, provider);
        async move { allowed }
    }

    async fn accounts(&self, provider: &str) -> Vec<Account> {
        {
            self.registry
                .get_credential_accounts(provider)
                .await
                .map(|accounts| {
                    accounts
                        .into_iter()
                        .filter(|a| !a.unused)
                        .map(|a| Account {
                            name: a.name,
                            pinned: a.pinned,
                        })
                        .collect()
                })
                .unwrap_or_default()
        }
    }

    async fn provider_auth(&self, provider: &str, slot: Option<&str>) -> Result<(), ProbeError> {
        {
            self.registry
                .get_provider_auth_for_slot(provider, slot)
                .await
                .map(|_| ())
                .map_err(probe_error)
        }
    }

    async fn request_auth(
        &self,
        provider: &str,
        model: &str,
        slot: Option<&str>,
    ) -> Result<(), ProbeError> {
        {
            let Some(found) = self.registry.find(provider, model) else {
                return Err(ProbeError {
                    class: Some("ModelNotFound".into()),
                    code: None,
                });
            };
            self.registry
                .get_api_key_and_headers_for_slot(&found, slot)
                .await
                .map(|_| ())
                .map_err(probe_error)
        }
    }
}

fn retired_logged() -> &'static Mutex<HashSet<String>> {
    static LOGGED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    LOGGED.get_or_init(Mutex::default)
}

fn log_line(message: &str) {
    maho_support::log(message, None);
    maho_ext_api::diagnostic(&format!("[model-profile] {message}"));
}

/// The session-start model-profile extension.
#[derive(Clone, Default)]
pub struct ModelProfileExtension;

impl Extension for ModelProfileExtension {
    fn register(&self, api: &mut ExtensionApi) {
        let sender = Arc::new(ExtensionApi::new(
            api.registered.clone(),
            api.profile.clone(),
            api.events.clone(),
            api.runtime.clone(),
        ));
        let applied = Arc::new(AppliedSessions::default());
        api.on(
            EventKind::SessionStart,
            Arc::new(move |event, ctx| {
                let sender = Arc::clone(&sender);
                let applied = Arc::clone(&applied);
                let ctx = ctx.clone();
                let event = event.clone();
                Box::pin(async move {
                    let ExtensionEvent::SessionStart(start) = &event else { return Ok(EventResult::None) };
                    if !should_apply(start.reason, start.initial_model_provenance.as_deref(), ctx.mode) {
                        return Ok(EventResult::None);
                    }
                    if !applied.first(ctx.session_manager.session_id()) {
                        return Ok(EventResult::None);
                    }
                    let loaded = maho_config::load_native(&ctx.cwd, &maho_config::process_env());
                    let view = &loaded.view;
                    let configured = view.get("model_profile").and_then(Value::as_str);
                    let model_profiles = view.get("model_profiles");
                    let profiles = merge_profiles(model_profiles);
                    let active = effective_profile(configured, model_profiles);
                    if configured.map(str::trim) == Some(RETIRED_PROFILE_ID)
                        && active.is_none()
                        && retired_logged()
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .insert(RETIRED_PROFILE_ID.to_owned())
                    {
                        log_line("the model profile \"recommended\" is retired; using the default lanes");
                    }
                    let available: Vec<String> = ctx
                        .model_registry
                        .get_available()
                        .iter()
                        .map(|m| format!("{}/{}", m.provider, m.id))
                        .collect();
                    let probe = RegistryProbe {
                        registry: Arc::clone(&ctx.model_registry),
                        agent_dir: ctx.agent_dir.clone(),
                    };
                    let (result, failures) = probe_and_resolve(&probe, |exclusions| match &active {
                        Some(name) => resolve_profile(&profiles, name, &available, exclusions),
                        None => resolve_default(&profiles, &available, exclusions),
                    })
                    .await;
                    for failure in &failures {
                        log_line(&format!(
                            "skipped {}/{} ({}, {})",
                            failure.provider,
                            failure.model,
                            failure.reason.as_str(),
                            failure.fingerprint
                        ));
                    }
                    let rpc = matches!(ctx.mode, ExtensionMode::Rpc);
                    if let ProfileResolution::Resolved {
                        provider,
                        model_id,
                        reasoning,
                        ..
                    } = &result
                    {
                        let Some(model) = ctx.model_registry.find(provider, model_id) else {
                            ctx.ui.notify(
                                &format!("mhc: the model profile resolved {provider}/{model_id} but the registry no longer lists it"),
                                NotificationType::Warning,
                            );
                            return Ok(EventResult::None);
                        };
                        match sender.set_session_model(model).await {
                            Ok(true) => {}
                            Ok(false) => return Ok(EventResult::None),
                            Err(error) => {
                                log_line(&format!("cannot set the session model: {}", error.message));
                                return Ok(EventResult::None);
                            }
                        }
                        if let Some(level) = thinking_level_for(reasoning.as_deref())
                            && let Err(error) = sender.set_session_model_thinking_level(level)
                        {
                            log_line(&format!("cannot set the session thinking level: {}", error.message));
                        }
                    }
                    let (custom_type, text, details) = notice_for(&result, &profiles, &failures, rpc);
                    if custom_type != APPLIED_TYPE {
                        log_line(&text);
                    }
                    let message = CustomMessage {
                        custom_type: custom_type.into(),
                        content: vec![maho_ext_api::ToolContent::text(text)],
                        display: true,
                        details: Some(details),
                    };
                    if let Err(error) = sender.send_message(message, SendMessageOptions::default()) {
                        log_line(&format!("cannot send the model profile notice: {}", error.message));
                    }
                    Ok(EventResult::None)
                })
            }),
        );
    }
}
