//! Port of omo-senpi `components/model-profile/index.ts` at omo `455dee62`: the `model-profile`
//! component.
//!
// allow: SIZE_OK - the repository's port rule is one senpi source file per Rust module, so this
// module stays the single translation of `index.ts` (the one module that owns the component's
// registration, gate, walk, apply and notice paths).
//!
//! Applies the active `model_profile` to the MAIN session model at session start.
//!
//! Session-scoped by construction: the apply path is the session-only model setter (senpi
//! `agent-session.ts` `persistDefault: false`), never the persisting one, which runs
//! `setDefaultModelAndProvider()` -> `settings.json` and would turn the profile into the very pin
//! that disables it on the next start. The component never reads `settings.json` either - senpi's
//! own `recommended-models` builtin and `/model` rewrite `defaultProvider`/`defaultModel` on the same
//! event, so those keys mean "last used", not "pinned". The pin lives in `model_profile` itself as a
//! literal `provider/model`.
//!
//! A rung counts only when the first turn could use it: [`crate::request_auth`] reproduces that
//! turn's credential resolution (per credential slot, then the model's own request configuration), so
//! a stored login that no longer refreshes drops its provider and a model whose headers do not
//! resolve drops only itself. The walk continues with the survivors.
//!
//! In-tier fallback is start-time only. Mid-session failures follow senpi's `retry.fallbackChains`
//! (keyed by model family); wiring those to the tier is a named follow-up, and the applied notice
//! says so.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use maho_ai::model::Model;
use maho_ai::types::ModelThinkingLevel;
use maho_ext_api::{
    ComponentLogger, EventKind, EventResult, Extension, ExtensionApi, ExtensionEvent, ExtensionMode,
    ExtensionRuntime, JsonValue, ModelRegistry, SendMessageOptions, SessionReason, ToolContent,
    CustomMessage,
};
use omo_config_core::LoadOmoConfigOptions;
use maho_omo_config_resolution::{SenpiOmoConfigResult, load_senpi_omo_config};

use crate::builtin_profiles::DEFAULT_MODEL_PROFILE_ID;
use crate::credential_policy::credential_rotation_policy;
use crate::notice::{auth_failed_details, notice_content};
use crate::request_auth::{AuthFailure, probe_request_auth, sanitized_auth_error_detail};
use crate::resolve::{
    ModelProfileResolution, ModelProfileSource, ResolveModelProfileInput, resolve_model_profile,
};

pub const MODEL_PROFILE_APPLIED_TYPE: &str = "omo-model-profile:applied";
pub const MODEL_PROFILE_UNAVAILABLE_TYPE: &str = "omo-model-profile:unavailable";
pub const MODEL_PROFILE_UNKNOWN_TYPE: &str = "omo-model-profile:unknown";

/// Upstream `ModelProfileComponentOptions.loadConfig`.
pub type LoadConfig = Arc<dyn Fn(&Path) -> SenpiOmoConfigResult + Send + Sync>;

/// Upstream `ModelProfileComponentOptions`.
///
/// `env` is the native injection seam for the default loader (the same pattern as
/// `SenpiTelemetryOptions.env` and `GitMasterAttributionComponentOptions.env`): production leaves it
/// `None`, which makes `load_senpi_omo_config` read the process environment exactly as upstream's
/// `loadSenpiOmoConfig({ cwd })` does; tests inject a temp `HOME` so resolution is deterministic.
#[derive(Clone, Default)]
pub struct ModelProfileComponentOptions {
    pub load_config: Option<LoadConfig>,
    pub env: Option<omo_config_core::OmoConfigEnv>,
}

/// Upstream `defaultLoadConfig`: `loadSenpiOmoConfig({ cwd })`.
pub fn default_load_config(
    cwd: &Path,
    env: Option<&omo_config_core::OmoConfigEnv>,
) -> SenpiOmoConfigResult {
    load_senpi_omo_config(LoadOmoConfigOptions {
        cwd: Some(cwd.to_string_lossy().into_owned()),
        env: env.cloned(),
        ..Default::default()
    })
}

/// Upstream `createModelProfileComponent`.
#[derive(Clone, Default)]
pub struct ModelProfileComponent {
    pub options: ModelProfileComponentOptions,
}

impl ModelProfileComponent {
    pub fn new(options: ModelProfileComponentOptions) -> Self {
        Self { options }
    }

    pub fn with_load_config(load_config: LoadConfig) -> Self {
        Self::new(ModelProfileComponentOptions { load_config: Some(load_config), env: None })
    }
}

/// Mirrors senpi-task's `asSenpiThinkingLevel`, which that package does not export publicly: omo.json
/// spells the disabled level "none" where senpi spells it "off", "auto" and unknown tokens leave the
/// session default alone.
pub fn as_senpi_thinking_level(reasoning: Option<&str>) -> Option<ModelThinkingLevel> {
    let reasoning = reasoning?;
    let normalized = if reasoning == "none" { "off" } else { reasoning };
    match normalized {
        "off" => Some(ModelThinkingLevel::Off),
        "minimal" => Some(ModelThinkingLevel::Minimal),
        "low" => Some(ModelThinkingLevel::Low),
        "medium" => Some(ModelThinkingLevel::Medium),
        "high" => Some(ModelThinkingLevel::High),
        "xhigh" => Some(ModelThinkingLevel::Xhigh),
        "max" => Some(ModelThinkingLevel::Max),
        _ => None,
    }
}

// Only a fresh session may receive the profile: a resume/fork carries its own model history, a
// reload keeps the running session, and a `--model` flag or scoped model is explicit user state.
// A session_start without any provenance is treated as explicit too: senpi omits the field on a
// `--model` run, and silently overriding an unknown origin would clobber the user's choice.
fn is_fresh_session_without_explicit_model(reason: SessionReason, provenance: Option<&str>) -> bool {
    if !matches!(reason, SessionReason::Startup | SessionReason::New) {
        return false;
    }
    provenance.is_some_and(|provenance| provenance != "cli" && provenance != "scoped")
}

/// Lanes have no TUI surface yet: the terminal shows neither the lane nor its reasoning, so an
/// interactive TUI session keeps the model the user started with. The desktop (rpc) and headless
/// runs still apply the profile.
fn is_tui_session(mode: ExtensionMode) -> bool {
    mode == ExtensionMode::Tui
}

fn mode_str(mode: ExtensionMode) -> Option<&'static str> {
    match mode {
        ExtensionMode::Tui => Some("tui"),
        ExtensionMode::Rpc => Some("rpc"),
        ExtensionMode::AppServer => Some("app-server"),
        ExtensionMode::Json => Some("json"),
        ExtensionMode::Print => Some("print"),
    }
}

fn available_selectors(registry: &dyn ModelRegistry) -> Vec<String> {
    registry
        .get_available()
        .into_iter()
        .map(|model| format!("{}/{}", model.provider, model.id))
        .collect()
}

fn provider_of(selector: &str) -> &str {
    selector.split_once('/').map_or(selector, |(provider, _)| provider)
}

fn log_skipped_candidate(logger: Option<&Arc<dyn ComponentLogger>>, failure: &AuthFailure) {
    let candidate = format!("{}/{}", failure.provider, failure.model);
    let message = format!(
        "omo-senpi: model profile skipped {candidate}: request auth did not resolve ({}, {})",
        failure.reason.as_str(),
        failure.error_kind
    );
    if let Some(logger) = logger {
        let details = JsonValue::Object(
            [
                ("provider".to_owned(), JsonValue::String(failure.provider.clone())),
                ("model".to_owned(), JsonValue::String(failure.model.clone())),
                ("reason".to_owned(), JsonValue::String(failure.reason.as_str().to_owned())),
                ("errorKind".to_owned(), JsonValue::String(failure.error_kind.clone())),
            ]
            .into_iter()
            .collect(),
        );
        logger.warn(&message, Some(&details));
        if std::env::var_os("OMO_DEBUG").is_some() {
            let detail = format!(
                "omo-senpi: model profile skipped {candidate}: {}",
                sanitized_auth_error_detail(failure.error.as_ref())
            );
            logger.info(&detail, None);
        }
    }
}

struct ProbedResolution {
    resolution: ModelProfileResolution,
    model: Option<Model>,
    failures: Vec<AuthFailure>,
}

// Each failed probe removes at least one selector (the whole provider for a credential failure, the
// one model for a request-configuration failure), so the walk ends after at most one pass per
// available selector. A literal pin is the user's explicit choice and is never swapped, so it is not
// probed. The native registry always exposes the probe port, so upstream's "host with no runtime"
// branch has no native counterpart.
async fn resolve_with_request_auth(
    registry: &dyn ModelRegistry,
    agent_dir: Option<&Path>,
    profiles: Option<&JsonValue>,
    active: &str,
    logger: Option<&Arc<dyn ComponentLogger>>,
) -> ProbedResolution {
    let available = available_selectors(registry);
    let mut excluded_providers: BTreeSet<String> = BTreeSet::new();
    let mut excluded_selectors: BTreeSet<String> = BTreeSet::new();
    let mut failures: Vec<AuthFailure> = Vec::new();
    // The rotation policy reads the registry's own auth-status port (a runtime API key turns
    // rotation off), never an injected stub.
    let policy = credential_rotation_policy(registry, agent_dir);
    let resolve = |excluded_providers: &BTreeSet<String>, excluded_selectors: &BTreeSet<String>| {
        let selectors: Vec<String> = available
            .iter()
            .filter(|selector| {
                !excluded_selectors.contains(*selector)
                    && !excluded_providers.contains(provider_of(selector))
            })
            .cloned()
            .collect();
        resolve_model_profile(&ResolveModelProfileInput {
            profiles,
            active,
            available_models: &selectors,
        })
    };

    let mut resolution = resolve(&excluded_providers, &excluded_selectors);
    let mut model: Option<Model> = None;
    while let ModelProfileResolution::Resolved { profile, provider, model_id, .. } = &resolution {
        let found = registry.find(provider, model_id);
        let is_pin = profile.source == ModelProfileSource::Pin;
        model = found;
        let Some(current) = model.clone() else {
            break;
        };
        if is_pin {
            break;
        }
        let failure = probe_request_auth(
            registry,
            policy.may_rotate(provider),
            provider,
            model_id,
            &current,
        )
        .await;
        let Some(failure) = failure else {
            break;
        };
        log_skipped_candidate(logger, &failure);
        if failure.reason == crate::request_auth::AuthFailureReason::Request {
            excluded_selectors.insert(format!("{}/{}", failure.provider, failure.model));
        } else {
            excluded_providers.insert(failure.provider.clone());
        }
        failures.push(failure);
        resolution = resolve(&excluded_providers, &excluded_selectors);
    }
    ProbedResolution { resolution, model, failures }
}

fn send_notice(
    runtime: &ExtensionRuntime,
    custom_type: &str,
    content: String,
    details: Option<JsonValue>,
    logger: Option<&Arc<dyn ComponentLogger>>,
) {
    let Ok(actions) = runtime.message_actions() else {
        return;
    };
    let message = CustomMessage {
        custom_type: custom_type.to_owned(),
        content: vec![ToolContent::text(content)],
        display: true,
        details,
    };
    if let Err(error) = actions.send_message(message, SendMessageOptions::default())
        && let Some(logger) = logger
    {
        logger.error("omo-senpi: model profile notice could not be sent", Some(&JsonValue::String(error.message)));
    }
}

impl Extension for ModelProfileComponent {
    fn register(&self, api: &mut ExtensionApi) {
        let runtime = api.runtime.clone();
        let env = self.options.env.clone();
        let load_config: LoadConfig = self.options.load_config.clone().unwrap_or_else(move || {
            Arc::new(move |cwd: &Path| default_load_config(cwd, env.as_ref()))
        });
        // One apply per session id; a host that reports no id gets exactly one apply per extension
        // instance, which is the conservative reading of "never clobber twice".
        let applied_sessions: Arc<Mutex<BTreeSet<String>>> = Arc::new(Mutex::new(BTreeSet::new()));

        api.on(
            EventKind::SessionStart,
            Arc::new(move |event, ctx| {
                let runtime = runtime.clone();
                let load_config = Arc::clone(&load_config);
                let applied_sessions = Arc::clone(&applied_sessions);
                Box::pin(async move {
                    let ExtensionEvent::SessionStart(start) = event else {
                        return Ok(EventResult::None);
                    };
                    if is_tui_session(ctx.mode)
                        || !is_fresh_session_without_explicit_model(
                            start.reason,
                            start.initial_model_provenance.as_deref(),
                        )
                    {
                        return Ok(EventResult::None);
                    }
                    let session_id = ctx.session_manager.session_id().to_owned();
                    if !applied_sessions
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .insert(session_id)
                    {
                        return Ok(EventResult::None);
                    }

                    let config = load_config(&ctx.cwd).config;
                    let configured = config.get("model_profile").and_then(JsonValue::as_str);
                    let active = configured
                        .filter(|value| !value.trim().is_empty())
                        .unwrap_or(DEFAULT_MODEL_PROFILE_ID)
                        .to_owned();
                    let profiles = config.get("model_profiles").cloned();

                    let Ok(actions) = runtime.session_actions() else {
                        if let Some(logger) = ctx.logger.as_ref() {
                            logger.warn(
                                "omo-senpi: model profile skipped - this runtime has no session model setter",
                                None,
                            );
                        }
                        return Ok(EventResult::None);
                    };

                    let mode = mode_str(ctx.mode);
                    let logger = ctx.logger.clone();
                    let probed = resolve_with_request_auth(
                        ctx.model_registry.as_ref(),
                        Some(ctx.agent_dir.as_path()),
                        profiles.as_ref(),
                        &active,
                        logger.as_ref(),
                    )
                    .await;
                    let content = notice_content(&probed.resolution, &probed.failures, mode);

                    if let ModelProfileResolution::Resolved { profile, provider, model_id, reasoning, skipped } =
                        &probed.resolution
                    {
                        let Some(model) = probed.model.clone() else {
                            let message = format!(
                                "OmO Native: model profile \"{}\" resolved {provider}/{model_id} but the registry no longer lists it",
                                profile.id
                            );
                            send_notice(&runtime, MODEL_PROFILE_UNAVAILABLE_TYPE, message.clone(), None, logger.as_ref());
                            if let Some(logger) = logger.as_ref() {
                                logger.warn(&message, None);
                            }
                            return Ok(EventResult::None);
                        };
                        // The session-only setter: `persistDefault: false`, so `settings.json` is
                        // never written and the profile never becomes a pin.
                        if let Err(error) = actions.set_session_model(model).await
                            && let Some(logger) = logger.as_ref()
                        {
                            logger.warn(&format!("omo-senpi: model profile could not set the session model ({})", error.message), None);
                        }
                        if let Some(level) = as_senpi_thinking_level(reasoning.as_deref())
                            && let Err(error) = actions.set_session_model_thinking_level(level)
                            && let Some(logger) = logger.as_ref()
                        {
                            logger.warn(&format!("omo-senpi: model profile could not set the session thinking level ({})", error.message), None);
                        }
                        let selected_model = format!("{provider}/{model_id}");
                        let mut details = serde_json::Map::new();
                        details.insert("profile".to_owned(), JsonValue::String(profile.id.clone()));
                        details.insert("model".to_owned(), JsonValue::String(selected_model.clone()));
                        details.insert(
                            "skipped".to_owned(),
                            JsonValue::Array(skipped.iter().cloned().map(JsonValue::String).collect()),
                        );
                        if let Some(reasoning) = reasoning {
                            details.insert("reasoning".to_owned(), JsonValue::String(reasoning.clone()));
                        }
                        if let Some(auth_failed) = auth_failed_details(&probed.failures)
                            && let JsonValue::Object(auth_failed) = auth_failed
                        {
                            details.extend(auth_failed);
                        }
                        send_notice(
                            &runtime,
                            MODEL_PROFILE_APPLIED_TYPE,
                            content.clone(),
                            Some(JsonValue::Object(details)),
                            logger.as_ref(),
                        );
                        if let Some(logger) = logger.as_ref() {
                            let log_details = JsonValue::Object(
                                [
                                    ("profile".to_owned(), JsonValue::String(profile.id.clone())),
                                    ("model".to_owned(), JsonValue::String(selected_model)),
                                ]
                                .into_iter()
                                .collect(),
                            );
                            logger.info(&content, Some(&log_details));
                        }
                        return Ok(EventResult::None);
                    }

                    let custom_type = match probed.resolution {
                        ModelProfileResolution::Unknown { .. } => MODEL_PROFILE_UNKNOWN_TYPE,
                        _ => MODEL_PROFILE_UNAVAILABLE_TYPE,
                    };
                    let details = match &probed.resolution {
                        ModelProfileResolution::Unavailable { profile, .. }
                            if !probed.failures.is_empty() =>
                        {
                            auth_failed_details(&probed.failures).map(|auth_failed| {
                                let mut map = serde_json::Map::new();
                                map.insert("profile".to_owned(), JsonValue::String(profile.id.clone()));
                                if let JsonValue::Object(auth_failed) = auth_failed {
                                    map.extend(auth_failed);
                                }
                                JsonValue::Object(map)
                            })
                        }
                        _ => None,
                    };
                    send_notice(&runtime, custom_type, content.clone(), details, logger.as_ref());
                    if let Some(logger) = logger.as_ref() {
                        logger.warn(&content, None);
                    }
                    Ok(EventResult::None)
                })
            }),
        );
    }
}
