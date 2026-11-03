//! The session-start model-profile extension (spec model-selection §6.7–6.8).

use crate::probe::{
    Account, AuthProbe, ProbeError, ProbeFailure, ProbeReason, probe_and_resolve_detailed,
    rotation_allowed_by_models_json,
};
use crate::profile::{
    ModelProfile, ProfileResolution, RETIRED_PROFILE_ID, effective_profile, merge_profiles,
    resolve_default, resolve_profile,
};
use maho_ai::types::ModelThinkingLevel;
use maho_ext_api::{
    EventKind, EventResult, Extension, ExtensionApi, ExtensionEvent, ExtensionFailure,
    ExtensionMode, ModelRegistry, NotificationType, SessionReason,
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

/// Providers named by an unavailable chain: every ranked provider of the profile's rungs, else
/// the provider prefixes of the chain labels (pins).
fn chain_providers(profile: &str, chain: &[String], profiles: &[ModelProfile]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |provider: &str| {
        if !provider.is_empty() && !out.iter().any(|p| p == provider) {
            out.push(provider.to_owned());
        }
    };
    match profiles.iter().find(|p| p.id == profile) {
        Some(found) => found
            .rungs
            .iter()
            .flat_map(|rung| rung.providers.iter())
            .for_each(|provider| push(provider.as_str())),
        None => chain
            .iter()
            .filter_map(|entry| entry.split_once('/'))
            .for_each(|(provider, _)| push(provider)),
    }
    out
}

/// The next step when no rung is in the registry (Delta D3.1), by run mode.
fn next_step(providers: &[String], rpc: bool) -> String {
    let listed = if providers.is_empty() {
        "a provider".to_owned()
    } else {
        format!("one of these providers ({})", providers.join(", "))
    };
    if rpc {
        format!(
            "Sign in to {listed} in the client's provider settings, or pick a model in the client."
        )
    } else {
        format!(
            "Log in to {listed} from an interactive session with /login, or start with --model."
        )
    }
}

/// The notice for a resolved model the registry no longer lists (Delta D3.2):
/// `(custom type, text, details)`.
pub fn registry_missing_notice(
    profile: &str,
    provider: &str,
    model_id: &str,
    rpc: bool,
) -> (&'static str, String, Value) {
    let how = if rpc {
        "in the settings"
    } else {
        "with /model or --model"
    };
    (
        UNAVAILABLE_TYPE,
        format!(
            "mhc: model profile {profile} resolved {provider}/{model_id}, but the registry no longer lists it; \
             the session keeps its current model. Choose another model for this profile {how}."
        ),
        json!({"profile": profile, "model": format!("{provider}/{model_id}"), "registryMissing": true}),
    )
}

/// What one profile outcome produces (Delta D2): a session entry that is not model input, a log
/// line, and at most one UI notification.
#[derive(Clone, Debug, PartialEq)]
pub struct OutcomeReport {
    pub custom_type: &'static str,
    pub text: String,
    pub details: Value,
    pub notification: Option<NotificationType>,
}

/// The notification level of an outcome (Delta D2).
pub fn outcome_notification(
    custom_type: &str,
    registry_missing: bool,
    from_default: bool,
    has_auth_failures: bool,
) -> Option<NotificationType> {
    if registry_missing {
        return Some(NotificationType::Warning);
    }
    if custom_type == APPLIED_TYPE {
        return has_auth_failures.then_some(NotificationType::Warning);
    }
    Some(if from_default && !has_auth_failures {
        NotificationType::Info
    } else {
        NotificationType::Warning
    })
}

/// The report for a resolution; `from_default` is true when the default order (§6.5) was used.
pub fn report_for(
    result: &ProfileResolution,
    profiles: &[ModelProfile],
    failures: &[ProbeFailure],
    rpc: bool,
    from_default: bool,
) -> OutcomeReport {
    let (custom_type, text, details) = notice_for(result, profiles, failures, rpc);
    OutcomeReport {
        custom_type,
        text,
        details,
        notification: outcome_notification(custom_type, false, from_default, !failures.is_empty()),
    }
}

/// The report for a resolved model the registry no longer lists.
pub fn registry_missing_report(
    profile: &str,
    provider: &str,
    model_id: &str,
    rpc: bool,
) -> OutcomeReport {
    let (custom_type, text, details) = registry_missing_notice(profile, provider, model_id, rpc);
    OutcomeReport {
        custom_type,
        text,
        details,
        notification: outcome_notification(custom_type, true, false, false),
    }
}

/// Where outcome reports go. There is deliberately no path to the model's input.
pub trait OutcomeHost {
    /// Appends a session entry that is not part of the model's input, as start-up bookkeeping
    /// (no session file for a session without a conversation). `Err` when the runtime cannot
    /// append entries.
    fn append_entry(&self, custom_type: &str, details: Value) -> Result<(), String>;
    /// Whether a UI notification channel exists (false for headless json/print runs).
    fn has_ui(&self) -> bool;
    fn notify(&self, text: &str, kind: NotificationType);
    fn log(&self, line: &str);
}

/// Delivers a report: entry + log line always (a failed append is logged), notification only
/// when the host has a UI.
pub fn deliver_report(host: &dyn OutcomeHost, report: &OutcomeReport) {
    if let Err(error) = host.append_entry(report.custom_type, report.details.clone()) {
        host.log(&format!("cannot record the model profile outcome: {error}"));
    }
    host.log(&report.text);
    if let Some(kind) = report.notification
        && host.has_ui()
    {
        host.notify(&report.text, kind);
    }
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
                "mhc: no model of profile {} is available (tried {}); keeping the current model.{}",
                label(profile),
                chain.join(", "),
                if failures.is_empty() {
                    format!(
                        " {}",
                        next_step(&chain_providers(profile, chain, profiles), rpc)
                    )
                } else {
                    tail
                }
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

/// Cross-lane prerequisite: `ExtensionFailure` does not yet carry the transient/refused OAuth
/// refresh split (MIT senpi port); until it does every `oauth` failure maps to a refused refresh.
fn probe_error(failure: ExtensionFailure) -> ProbeError {
    ProbeError {
        class: failure.class,
        code: failure.code,
        ..ProbeError::default()
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
                    ..ProbeError::default()
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

/// [`OutcomeHost`] over the extension API and context.
struct ExtensionOutcomeHost<'a> {
    api: &'a ExtensionApi,
    ctx: &'a maho_ext_api::ExtensionContext,
}

impl OutcomeHost for ExtensionOutcomeHost<'_> {
    fn append_entry(&self, custom_type: &str, details: Value) -> Result<(), String> {
        // Cross-lane prerequisite: the senpi `appendEntry` start-up-bookkeeping option is not on
        // mhc's `append_entry` yet; once ported it must be passed here.
        self.api
            .append_entry(custom_type, Some(details))
            .map_err(|error| error.message)
    }
    fn has_ui(&self) -> bool {
        self.ctx.has_ui
    }
    fn notify(&self, text: &str, kind: NotificationType) {
        self.ctx.ui.notify(text, kind);
    }
    fn log(&self, line: &str) {
        log_line(line);
    }
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
                    let ExtensionEvent::SessionStart(start) = &event else {
                        return Ok(EventResult::None);
                    };
                    if !should_apply(
                        start.reason,
                        start.initial_model_provenance.as_deref(),
                        ctx.mode,
                    ) {
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
                        log_line(
                            "the model profile \"recommended\" is retired; using the default lanes",
                        );
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
                    let outcome = probe_and_resolve_detailed(&probe, |exclusions| match &active {
                        Some(name) => resolve_profile(&profiles, name, &available, exclusions),
                        None => resolve_default(&profiles, &available, exclusions),
                    })
                    .await;
                    for kept in &outcome.kept {
                        log_line(&kept.warning_line());
                    }
                    let (result, failures) = (outcome.result, outcome.failures);
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
                        profile,
                        provider,
                        model_id,
                        reasoning,
                        ..
                    } = &result
                    {
                        let Some(model) = ctx.model_registry.find(provider, model_id) else {
                            deliver_report(
                                &ExtensionOutcomeHost {
                                    api: &sender,
                                    ctx: &ctx,
                                },
                                &registry_missing_report(profile, provider, model_id, rpc),
                            );
                            return Ok(EventResult::None);
                        };
                        match sender.set_session_model(model).await {
                            Ok(true) => {}
                            Ok(false) => return Ok(EventResult::None),
                            Err(error) => {
                                log_line(&format!(
                                    "cannot set the session model: {}",
                                    error.message
                                ));
                                return Ok(EventResult::None);
                            }
                        }
                        if let Some(level) = thinking_level_for(reasoning.as_deref())
                            && let Err(error) = sender.set_session_model_thinking_level(level)
                        {
                            log_line(&format!(
                                "cannot set the session thinking level: {}",
                                error.message
                            ));
                        }
                    }
                    deliver_report(
                        &ExtensionOutcomeHost {
                            api: &sender,
                            ctx: &ctx,
                        },
                        &report_for(&result, &profiles, &failures, rpc, active.is_none()),
                    );
                    Ok(EventResult::None)
                })
            }),
        );
    }
}
