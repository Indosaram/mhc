//! Port of `packages/omo-senpi/src/components/telemetry/omo-native-component.ts`.
//!
//! The registration order is behaviour: the turn-span shutdown runs before the session client's, the parallel
//! and summary handlers must precede the session component's shutdown (which clears the capture facade), and
//! the delegation observer is detached on shutdown so a re-register cannot pile observers up.

use crate::{
    index::SenpiTelemetryOptions,
    omo_native_consent::NativeConsent,
    omo_native_parallel::ParallelTelemetryRegistry,
    omo_native_parallel_summary::{SummaryCapture, register_omo_native_parallel_summary},
    omo_native_session::{
        OPTED_OUT_SESSION_HASH, StartNativeSessionInput, start_native_session,
    },
    omo_native_summary_wiring::{
        NativeCaptureState, NativeSummaryRecorderInput, SUMMARY_SHUTDOWN_WAIT_MS,
        create_native_summary_recorder, settle_within,
    },
    omo_native_turn_span::{OmoNativeTurnSpansOptions, create_omo_native_turn_spans},
    omo_native_turns::{OmoNativeTurnHandlerOptions, handle_turn_end},
    product_identity::{OMO_NATIVE_PROPERTY_ALLOWLISTS, get_omo_native_state_dir},
    session_role_resolver::{
        is_telemetry_host_warmup, read_session_traceparent, resolve_telemetry_session_role,
        session_role_name,
    },
    session_summary_legacy_rollups::remove_legacy_rollups_once,
    session_summary_optout_start::{StartOptOutPurgeInput, start_opt_out_purge},
};
use futures::FutureExt;
use maho_ext_api::{
    BusSubscription, EventKind, EventResult, Extension, ExtensionApi, ExtensionEvent, SessionReason,
};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex, PoisonError},
};

pub type ConfigEnabled = Arc<dyn Fn(&Path) -> bool + Send + Sync>;

pub fn warn(ctx: &maho_ext_api::ExtensionContext, message: &str) {
    match &ctx.logger {
        Some(logger) => logger.warn(message, None),
        None => maho_ext_api::diagnostic(message),
    }
}

pub struct OmoNativeTelemetryComponent {
    pub options: SenpiTelemetryOptions,
    pub skills_root: PathBuf,
    pub is_config_enabled: ConfigEnabled,
    pub clock: Arc<dyn Fn() -> f64 + Send + Sync>,
    subscriptions: Mutex<Vec<BusSubscription>>,
}

impl OmoNativeTelemetryComponent {
    pub fn new(
        mut options: SenpiTelemetryOptions,
        skills_root: PathBuf,
        is_config_enabled: ConfigEnabled,
    ) -> Self {
        options.is_config_enabled = Some(Arc::clone(&is_config_enabled));
        Self {
            options,
            skills_root,
            is_config_enabled,
            clock: Arc::new(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs_f64()
                    * 1000.0
            }),
            subscriptions: Mutex::default(),
        }
    }
}

impl OmoNativeTelemetryComponent {
    pub fn with_default_config(options: SenpiTelemetryOptions, skills_root: PathBuf) -> Self {
        let env = options
            .env
            .clone()
            .unwrap_or_else(|| std::env::vars().collect());
        let diagnostics = options.diagnostics.clone();
        let enabled = Arc::new(move |cwd: &std::path::Path| {
            let loaded = maho_omo_config_resolution::load_senpi_omo_config(
                omo_config_core::LoadOmoConfigOptions {
                    cwd: Some(cwd.to_string_lossy().into_owned()),
                    env: Some(env.iter().map(|(k, v)| (k.clone(), v.clone())).collect()),
                    ..Default::default()
                },
            );
            if !loaded.diagnostics.is_empty() {
                if let Some(diagnostics) = &diagnostics {
                    diagnostics(&telemetry_core::TelemetryDiagnosticInput {
                        event: telemetry_core::TelemetryDiagnosticEvent::TelemetryCaptureFailed,
                        source: "omo-native-session".into(),
                        error: Some(telemetry_core::TelemetryError::new(format!(
                            "{:?}",
                            loaded.diagnostics
                        ))),
                        error_kind: Some(telemetry_core::TelemetryDiagnosticErrorKind::Error),
                        entry_kind: None,
                        error_code: None,
                        count: None,
                    });
                }
                return false;
            }
            omo_config_core::is_omo_telemetry_enabled(&loaded.config)
        });
        Self::new(options, skills_root, enabled)
    }
}

/// The session hasher every registration shares: an opted-out session hashes to the sentinel and never reads
/// or creates the salt.
fn session_hasher(
    consent: &Arc<NativeConsent>,
    state_dir: &Path,
) -> Arc<dyn Fn(&str) -> String + Send + Sync> {
    let consent = Arc::clone(consent);
    let state_dir = state_dir.to_path_buf();
    Arc::new(move |raw_id: &str| consent.hash_session(raw_id, &state_dir))
}

struct TurnSpanContext {
    role: String,
    traceparent: Option<String>,
}

fn turn_span_context(
    profile: &maho_ext_api::ExtensionSessionProfile,
    env: &telemetry_core::TelemetryEnv,
) -> TurnSpanContext {
    TurnSpanContext {
        role: session_role_name(Some(profile), env).to_string(),
        traceparent: read_session_traceparent(Some(profile), env),
    }
}

fn context_model_of(
    ctx: &maho_ext_api::ExtensionContext,
) -> Option<crate::omo_native_turn_span::TurnModel> {
    let model = ctx.model.as_ref()?;
    Some(crate::omo_native_turn_span::TurnModel {
        provider: model.provider.clone(),
        model_id: model.id.clone(),
    })
}

type TurnResponse = (
    Option<crate::omo_native_turn_span::TurnModel>,
    Option<String>,
    bool,
);

fn turn_response(event: &ExtensionEvent) -> TurnResponse {
    let ExtensionEvent::TurnEnd { message, .. } = event else {
        return (None, None, false);
    };
    let value = serde_json::to_value(message).unwrap_or(serde_json::Value::Null);
    let provider = value.get("provider").and_then(|value| value.as_str());
    let model = value.get("model").and_then(|value| value.as_str());
    let response_model = value.get("responseModel").and_then(|value| value.as_str());
    let failed = value.get("stopReason").and_then(|value| value.as_str()) == Some("error");
    (
        provider
            .zip(model)
            .map(|(provider, model_id)| crate::omo_native_turn_span::TurnModel {
                provider: provider.to_string(),
                model_id: model_id.to_string(),
            }),
        response_model.map(str::to_string),
        failed,
    )
}

impl Extension for OmoNativeTelemetryComponent {
    fn register(&self, api: &mut ExtensionApi) {
        if api.runtime.get_flag("omo-senpi-telemetry-disabled")
            == Some(maho_ext_api::FlagValue::Boolean(true))
        {
            return;
        }
        // The throwaway session that warms a fresh task host is no session of anyone's: it must not be
        // counted as one, nor report the crashes and daily activity the real session reports.
        if is_telemetry_host_warmup(Some(&api.profile)) {
            return;
        }
        let env = self
            .options
            .env
            .clone()
            .unwrap_or_else(|| std::env::vars().collect());
        let state_dir = self
            .options
            .state_dir
            .clone()
            .unwrap_or_else(|| get_omo_native_state_dir(&env));
        // Pre-#10225 rollups have no reader and no sender left: they go for every upgrader, whatever the
        // consent.
        remove_legacy_rollups_once(&state_dir, self.options.diagnostics.as_ref());

        let consent = NativeConsent::new(&self.options);
        consent.set_cwd(&api.cwd);
        let session_role = resolve_telemetry_session_role(Some(&api.profile), &env);
        let consented_at_registration = consent.is_consented(true);
        // An opted-out process deletes what dead processes left for it at once, not only when it quits.
        if !consented_at_registration {
            // Consent here is the persisted setting only: an env opt-out is this process's own and must not
            // delete what consented sessions queued.
            let purge_consent = Arc::clone(&consent);
            let persisted_opt_out = consent.persisted_opt_out();
            start_opt_out_purge(StartOptOutPurgeInput {
                state_dir: state_dir.clone(),
                is_scope_consented: Arc::new(move |scope| {
                    purge_consent.is_queue_scope_consented(scope)
                }),
                persisted_opt_out,
                is_pid_alive: self.options.is_pid_alive.clone(),
            });
        }

        let state = Arc::new(NativeCaptureState::default());
        *state
            .is_consented
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(Arc::new({
            let consent = Arc::clone(&consent);
            move || consent.is_consented(false)
        }));
        *state
            .is_send_allowed
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(Arc::new({
            let consent = Arc::clone(&consent);
            move || consent.is_consented(true)
        }));
        consent.set_drop_queued(Arc::new({
            let state = Arc::clone(&state);
            move || {
                let drop_queued = state
                    .drop_queued
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone();
                if let Some(drop_queued) = drop_queued {
                    drop_queued();
                }
            }
        }));

        // Every event this session emits goes through here, so one emitted while a turn is open carries that
        // turn's span ids.
        let span_context = turn_span_context(&api.profile, &env);
        let turn_spans = create_omo_native_turn_spans(OmoNativeTurnSpansOptions {
            traces: {
                let state = Arc::clone(&state);
                Arc::new(move || state.traces())
            },
            is_consented: {
                let consent = Arc::clone(&consent);
                Arc::new(move || consent.is_consented(false))
            },
            is_send_allowed: {
                let consent = Arc::clone(&consent);
                Arc::new(move || consent.is_consented(true))
            },
            session_role: span_context.role,
            inherited_traceparent: span_context.traceparent,
        });
        let session_capture: SummaryCapture = {
            let state = Arc::clone(&state);
            let consent = Arc::clone(&consent);
            Arc::new(move |name, properties| {
                if !consent.is_consented(false) {
                    return;
                }
                if properties.get("$session_id").and_then(|value| value.as_str())
                    == Some(OPTED_OUT_SESSION_HASH)
                {
                    return;
                }
                if !OMO_NATIVE_PROPERTY_ALLOWLISTS.contains_key(name) {
                    return;
                }
                let Some(properties) = properties.as_object().cloned() else {
                    return;
                };
                if let Some(capture) = state.capture() {
                    let _ignored = capture(name, properties, None);
                }
            })
        };
        let session_client: SummaryCapture = {
            let session_capture = Arc::clone(&session_capture);
            let turn_spans = Arc::clone(&turn_spans);
            Arc::new(move |name, properties| {
                turn_spans.run(|| session_capture(name, properties));
            })
        };

        // A turn still open at shutdown must end before the session component's handler shuts the transport
        // down, or its span lands on a closed sink. Registered first.
        {
            let turn_spans = Arc::clone(&turn_spans);
            let consent = Arc::clone(&consent);
            api.on(
                EventKind::SessionShutdown,
                Arc::new(move |_, ctx| {
                    consent.set_cwd(&ctx.cwd);
                    turn_spans.shutdown();
                    Box::pin(async { Ok(EventResult::None) })
                }),
            );
        }

        // Must precede the session component: its shutdown handler shuts the client down, which clears the
        // capture facade, after which `parallelism_summary` would capture nothing.
        let registry = Arc::new(Mutex::new(ParallelTelemetryRegistry::default()));
        let subscription = register_omo_native_parallel_summary(
            api,
            registry,
            Arc::clone(&self.clock),
            session_hasher(&consent, &state_dir),
            Arc::clone(&session_client),
        );
        self.subscriptions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(subscription);

        let summary = consented_at_registration.then(|| {
            create_native_summary_recorder(&NativeSummaryRecorderInput {
                state: Arc::clone(&state),
                consent: Arc::clone(&consent),
                state_dir: state_dir.clone(),
                env: env.clone(),
                cwd: Some(api.cwd.clone()),
                session_role,
                raw_turn_sample_rate: self.options.raw_turn_sample_rate,
                is_pid_alive: self.options.is_pid_alive.clone(),
                diagnostics: self.options.diagnostics.clone(),
            })
        });

        // The final summary window must be captured before the session component's shutdown handler clears
        // the capture facade.
        {
            let summary = summary.clone();
            let consent = Arc::clone(&consent);
            let hash = session_hasher(&consent, &state_dir);
            let purge_state = state_dir.clone();
            let options = self.options.clone();
            api.on(
                EventKind::SessionShutdown,
                Arc::new(move |event, ctx| {
                    consent.set_cwd(&ctx.cwd);
                    let reason = match event {
                        ExtensionEvent::SessionShutdown(event)
                            if event.reason == SessionReason::Reload =>
                        {
                            Some("reload")
                        }
                        _ => Some("shutdown"),
                    };
                    let session_hash = hash(ctx.session_manager.session_id());
                    let summary = summary.clone();
                    let consent = Arc::clone(&consent);
                    let options = options.clone();
                    let purge_state = purge_state.clone();
                    Box::pin(async move {
                        match summary {
                            Some(summary) => {
                                let work = async move {
                                    summary.finish(&session_hash, reason).await;
                                    summary.dispose();
                                };
                                settle_within(Box::pin(work), SUMMARY_SHUTDOWN_WAIT_MS).await;
                            }
                            None => {
                                let persisted_opt_out = consent.persisted_opt_out();
                                start_opt_out_purge(StartOptOutPurgeInput {
                                    state_dir: purge_state,
                                    is_scope_consented: Arc::new(move |scope| {
                                        consent.is_queue_scope_consented(scope)
                                    }),
                                    persisted_opt_out,
                                    is_pid_alive: options.is_pid_alive.clone(),
                                });
                            }
                        }
                        Ok(EventResult::None)
                    })
                }),
            );
        }

        crate::omo_native_prompt::register_omo_native_prompt_telemetry(
            api,
            session_hasher(&consent, &state_dir),
            Arc::clone(&session_client),
        );

        crate::omo_native_handoff_hint::register_omo_native_handoff_hint_telemetry(
            api,
            Arc::clone(&session_capture),
            session_hasher(&consent, &state_dir),
        );

        // Task terminals are observed for the whole process lifetime of this registration, not per session
        // event: a background child can settle long after the turn that spawned it.
        let delegation = Arc::new(Mutex::new(Some(
            crate::omo_native_delegation::create_omo_native_delegation_capture(
                maho_omo_task::component::shared_task_terminal_observers(),
                Arc::clone(&session_client),
                session_hasher(&consent, &state_dir),
                {
                    let consent = Arc::clone(&consent);
                    Arc::new(move || consent.is_consented(false))
                },
                self.options.diagnostics.clone(),
            ),
        )));
        {
            let delegation = Arc::clone(&delegation);
            api.on(
                EventKind::SessionShutdown,
                Arc::new(move |_, _| {
                    delegation
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .take();
                    Box::pin(async { Ok(EventResult::None) })
                }),
            );
        }

        // The session component itself: session_start builds the client, installs the capture facade, and
        // reports the inventory, crashes, daily activity and category config.
        {
            let options = self.options.clone();
            let state = Arc::clone(&state);
            let consent = Arc::clone(&consent);
            let hash = session_hasher(&consent, &state_dir);
            let session_capture = Arc::clone(&session_capture);
            let categories = Arc::new(Mutex::new(
                crate::omo_native_category_config::CategoryConfigCapture::default(),
            ));
            let summary = summary.clone();
            let client: Arc<Mutex<Option<Arc<telemetry_core::EventTelemetryClient>>>> =
                Arc::default();
            let shared_client = Arc::clone(&client);
            let shutdown_state = Arc::clone(&state);
            api.on(EventKind::SessionStart, Arc::new(move |event, ctx| {
                consent.set_cwd(&ctx.cwd);
                if consent.is_consented(true)
                    && let ExtensionEvent::SessionStart(event) = event
                {
                    let reason = match event.reason {
                        SessionReason::Startup => "startup",
                        SessionReason::Reload => "reload",
                        SessionReason::New => "new",
                        SessionReason::Resume => "resume",
                        SessionReason::Fork => "fork",
                        SessionReason::Quit => "startup",
                    };
                    let payload = serde_json::json!({"reason": reason});
                    match start_native_session(&StartNativeSessionInput {
                        options: &options,
                        session_id: ctx.session_manager.session_id(),
                        payload: &payload,
                        agent_dir: &ctx.agent_dir,
                        session_role,
                    }) {
                        Ok(session) => {
                            let session = session.map(Arc::new);
                            if let Some(session) = &session {
                                let facade = Arc::clone(session);
                                state.set_capture(Arc::new(
                                    move |name: &str,
                                          properties: serde_json::Map<
                                        String,
                                        serde_json::Value,
                                    >,
                                          options: Option<
                                        telemetry_core::EventTelemetryCaptureOptions,
                                    >| {
                                        match options {
                                            Some(options) => {
                                                facade.capture_immediate(name, &properties, &options)
                                            }
                                            None => {
                                                facade.capture_event_with_options(
                                                    name, &properties, None,
                                                );
                                                None
                                            }
                                        }
                                    },
                                ));
                                let flush_client = Arc::clone(session);
                                *state
                                    .flush
                                    .lock()
                                    .unwrap_or_else(PoisonError::into_inner) =
                                    Some(Arc::new(move || {
                                        let flush_client = Arc::clone(&flush_client);
                                        async move { flush_client.flush().await }.boxed()
                                    }));
                            }
                            let env = options
                                .env
                                .clone()
                                .unwrap_or_else(|| std::env::vars().collect());
                            let session_hash = hash(ctx.session_manager.session_id());
                            *state
                                .session_hash
                                .lock()
                                .unwrap_or_else(PoisonError::into_inner) =
                                Some(session_hash.clone());
                            match categories
                                .lock()
                                .unwrap_or_else(PoisonError::into_inner)
                                .observe(ctx, &env, &session_hash, reason)
                            {
                                Ok(Some(properties)) => {
                                    session_capture("category_config", properties)
                                }
                                Ok(None) => {}
                                Err(error) => warn(
                                    ctx,
                                    &format!("omo-native category config failed: {error}"),
                                ),
                            }
                            if consent.is_consented(false) {
                                let crash_state = options
                                    .state_dir
                                    .clone()
                                    .unwrap_or_else(|| get_omo_native_state_dir(&env));
                                match crate::process_crash_records::claim_unreported_crash_records(
                                    &ctx.agent_dir,
                                    &crash_state,
                                    options.now.unwrap_or_else(chrono::Utc::now),
                                ) {
                                    Ok(records) => {
                                        for record in records {
                                            session_capture(
                                                "process_crashed",
                                                crate::omo_native_crash::process_crashed_properties(
                                                    &record,
                                                    options.os_provider.as_deref().unwrap_or(
                                                        telemetry_core::get_default_telemetry_os_provider(),
                                                    ),
                                                ),
                                            );
                                        }
                                    }
                                    Err(error) => warn(
                                        ctx,
                                        &format!("omo-native crash capture failed: {error}"),
                                    ),
                                }
                            }
                            *shared_client
                                .lock()
                                .unwrap_or_else(PoisonError::into_inner) = session;
                            // The first capture is when recovery runs: the capture path is installed by
                            // now, so a recovered window can actually be published.
                            if let Some(summary) = summary.as_ref() {
                                let summary = Arc::clone(summary);
                                tokio::spawn(async move {
                                    summary.recover().await;
                                });
                            }
                        }
                        Err(error) => {
                            warn(
                                ctx,
                                &format!("telemetry_capture_failed: omo-native-session: {error}"),
                            );
                        }
                    }
                }
                Box::pin(async { Ok(EventResult::None) })
            }));

            // The session client's own teardown: clears the facade, then shuts the transport down.
            let state = shutdown_state;
            api.on(EventKind::SessionShutdown, Arc::new(move |_, _| {
                let client = client
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take();
                state.clear_capture();
                Box::pin(async move {
                    if let Some(client) = client {
                        client.shutdown().await;
                    }
                    Ok(EventResult::None)
                })
            }));
        }

        {
            let turn_spans = Arc::clone(&turn_spans);
            let consent = Arc::clone(&consent);
            let hash = session_hasher(&consent, &state_dir);
            let state = Arc::clone(&state);
            api.on(EventKind::TurnStart, Arc::new(move |event, ctx| {
                if let ExtensionEvent::TurnStart { turn_index, .. } = event {
                    consent.set_cwd(&ctx.cwd);
                    let session_hash = state
                        .session_hash
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .clone()
                        .unwrap_or_else(|| hash(ctx.session_manager.session_id()));
                    turn_spans.start(
                        *turn_index as f64,
                        &session_hash,
                        Some(ctx.session_manager.session_id()),
                        context_model_of(ctx).as_ref(),
                    );
                }
                Box::pin(async { Ok(EventResult::None) })
            }));
        }

        {
            let turn_spans = Arc::clone(&turn_spans);
            let consent = Arc::clone(&consent);
            let summary = summary.clone();
            let diagnostics = self.options.diagnostics.clone();
            let hash = session_hasher(&consent, &state_dir);
            let session_capture = Arc::clone(&session_capture);
            api.on(EventKind::TurnEnd, Arc::new(move |event, ctx| {
                consent.set_cwd(&ctx.cwd);
                let session_hash = hash(ctx.session_manager.session_id());
                let response = turn_response(event);
                let payload = match event {
                    ExtensionEvent::TurnEnd {
                        turn_index,
                        message,
                        tool_results,
                    } => Some(serde_json::json!({
                        "turnIndex": turn_index,
                        "message": message,
                        "toolResults": tool_results,
                    })),
                    _ => None,
                };
                let summary = summary.clone();
                let capture = Arc::clone(&session_capture);
                let diagnostics = diagnostics.clone();
                let consent = Arc::clone(&consent);
                let turn_spans = Arc::clone(&turn_spans);
                Box::pin(async move {
                    if let (Some(summary), Some(payload)) = (summary, payload) {
                        let handler = OmoNativeTurnHandlerOptions {
                            capture,
                            diagnostics,
                            is_consented: Arc::new(move || consent.is_consented(false)),
                            session_id: session_hash,
                            session_role,
                            summary,
                        };
                        turn_spans
                            .run_async(handle_turn_end(&handler, &payload))
                            .await;
                    }
                    turn_spans.end(response.0.as_ref(), response.1.as_deref(), response.2);
                    Ok(EventResult::None)
                })
            }));
        }

        {
            let turn_spans = Arc::clone(&turn_spans);
            api.on(EventKind::AgentEnd, Arc::new(move |_, _| {
                turn_spans.abandon();
                Box::pin(async { Ok(EventResult::None) })
            }));
        }

        crate::omo_native_tools::register_omo_native_tool_telemetry(
            api,
            self.skills_root.clone(),
            session_hasher(&consent, &state_dir),
            Arc::clone(&session_client),
            Some(Arc::new(|ctx, message, details| {
                warn(ctx, &format!("{message}: {details}"))
            })),
        );
        crate::omo_native_notice::register_omo_native_notice(
            api,
            env,
            state_dir,
            Arc::clone(&self.is_config_enabled),
            self.options.diagnostics.clone(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry_test_support::*;
    use futures::future::BoxFuture;
    use telemetry_core::*;

    struct Recorder(Arc<Mutex<Vec<TelemetryCaptureMessage>>>);
    impl TelemetryTransport for Recorder {
        fn capture(&self, m: &TelemetryCaptureMessage) -> Result<(), TelemetryError> {
            self.0.lock().unwrap().push(m.clone());
            Ok(())
        }
        fn capture_immediate<'a>(
            &'a self,
            m: &'a TelemetryCaptureMessage,
        ) -> Option<
            BoxFuture<'a, Result<(), telemetry_core::TelemetryDeliveryError>>,
        > {
            self.0.lock().unwrap().push(m.clone());
            Some(Box::pin(async { Ok(()) }))
        }
        fn flush(&self) -> Option<BoxFuture<'_, Result<(), TelemetryError>>> {
            None
        }
        fn shutdown(&self) -> BoxFuture<'_, Result<(), TelemetryError>> {
            Box::pin(async { Ok(()) })
        }
    }

    fn component(
        home: &std::path::Path,
    ) -> (
        OmoNativeTelemetryComponent,
        Arc<Mutex<Vec<TelemetryCaptureMessage>>>,
    ) {
        std::fs::write(home.join("models.json"), "{\"providers\":{}}").unwrap();
        std::fs::write(home.join("settings.json"), "{}").unwrap();
        let messages = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&messages);
        let options = SenpiTelemetryOptions {
            env: Some(env(home)),
            state_dir: Some(home.join("native")),
            transport_factory: Some(Arc::new(move |_, _| {
                Ok(Box::new(Recorder(Arc::clone(&captured))))
            })),
            ..Default::default()
        };
        {
            let mut component =
                OmoNativeTelemetryComponent::new(options, home.join("skills"), Arc::new(|_| true));
            let clock = std::sync::atomic::AtomicU64::new(1000);
            component.clock = Arc::new(move || {
                f64::from(
                    u32::try_from(clock.fetch_add(1, std::sync::atomic::Ordering::SeqCst)).unwrap(),
                )
            });
            (component, messages)
        }
    }

    fn start() -> ExtensionEvent {
        ExtensionEvent::SessionStart(maho_ext_api::SessionStartEvent {
            reason: SessionReason::Startup,
            initial_model_provenance: None,
            previous_session_file: None,
            attended: None,
        })
    }

    fn shutdown() -> ExtensionEvent {
        ExtensionEvent::SessionShutdown(maho_ext_api::SessionShutdownEvent {
            reason: SessionReason::Quit,
            target_session_file: None,
            signal: None,
        })
    }

    fn session_started(
        messages: &Arc<Mutex<Vec<TelemetryCaptureMessage>>>,
    ) -> telemetry_core::TelemetryCaptureProperties {
        messages
            .lock()
            .unwrap()
            .iter()
            .find(|m| m.event == "session_started")
            .map(|m| m.properties.clone())
            .expect("session_started")
    }

    #[test]
    fn default_config_uses_owner_resolver_and_reports_invalid_config() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir(t.path().join(".omo")).unwrap();
        let config = t.path().join(".omo/omo.json");
        let diagnostics = Arc::new(Mutex::new(Vec::new()));
        let observed = Arc::clone(&diagnostics);
        let options = SenpiTelemetryOptions {
            env: Some(telemetry_core::TelemetryEnv::from([
                ("HOME".into(), t.path().to_string_lossy().into_owned()),
                (
                    "USERPROFILE".into(),
                    t.path().to_string_lossy().into_owned(),
                ),
            ])),
            diagnostics: Some(Arc::new(move |d| observed.lock().unwrap().push(d.clone()))),
            ..Default::default()
        };
        let component =
            OmoNativeTelemetryComponent::with_default_config(options, t.path().join("skills"));
        // Telemetry is opt-in (see docs/divergences.md): no config file means disabled.
        assert!(!(component.is_config_enabled)(t.path()));
        std::fs::write(&config, "{\"telemetry\":{\"enabled\":true}}").unwrap();
        assert!((component.is_config_enabled)(t.path()));
        std::fs::write(&config, "{\"telemetry\":{\"enabled\":false}}").unwrap();
        assert!(!(component.is_config_enabled)(t.path()));
        std::fs::write(&config, "{bad").unwrap();
        assert!(!(component.is_config_enabled)(t.path()));
        let diagnostics = diagnostics.lock().unwrap();
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(
            diagnostics[0].event,
            TelemetryDiagnosticEvent::TelemetryCaptureFailed
        );
        assert_eq!(diagnostics[0].source, "omo-native-session");
    }

    #[tokio::test]
    async fn corrupt_inventory_reports_once_omits_defaults() {
        let t = tempfile::tempdir().unwrap();
        let (mut component, messages) = component(t.path());
        std::fs::write(t.path().join("models.json"), "{not json").unwrap();
        std::fs::write(
            t.path().join("settings.json"),
            "{\"defaultProvider\":\"anthropic\",\"defaultModel\":\"\"}",
        )
        .unwrap();
        let diagnostics = Arc::new(Mutex::new(Vec::new()));
        let observed = Arc::clone(&diagnostics);
        component.options.inventory_diagnostics = Some(Arc::new(move |d| {
            observed.lock().unwrap().push((d.event, d.source))
        }));
        let mut api = api();
        component.register(&mut api);
        let ctx = context(t.path(), "corrupt");
        dispatch(&api, start(), &ctx).await;
        dispatch(&api, shutdown(), &ctx).await;
        let p = session_started(&messages);
        assert_eq!(p["provider_count"], 0);
        assert_eq!(p["model_count"], 0);
        assert!(!p.contains_key("default_provider"));
        assert!(!p.contains_key("default_model"));
        assert_eq!(
            *diagnostics.lock().unwrap(),
            vec![("omo_native_inventory_read_failed", "omo-native-session")]
        );
    }

    #[tokio::test]
    async fn absent_inventory_reports_once() {
        let t = tempfile::tempdir().unwrap();
        let (mut component, messages) = component(t.path());
        std::fs::remove_file(t.path().join("models.json")).unwrap();
        let diagnostics = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = Arc::clone(&diagnostics);
        component.options.inventory_diagnostics = Some(Arc::new(move |_| {
            observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));
        let mut api = api();
        component.register(&mut api);
        let ctx = context(t.path(), "absent");
        dispatch(&api, start(), &ctx).await;
        dispatch(&api, shutdown(), &ctx).await;
        assert_eq!(diagnostics.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(session_started(&messages)["providers"], "");
    }

    #[tokio::test]
    async fn session_inventory_privacy_exact_keys() {
        let t = tempfile::tempdir().unwrap();
        let (component, messages) = component(t.path());
        std::fs::write(t.path().join("models.json"),serde_json::json!({"providers":{"anthropic":{"models":[{"id":""},{"id":"private-model"}]},"private_cloud":{"models":[{"id":"secret-model"}]}}}).to_string()).unwrap();
        std::fs::write(
            t.path().join("settings.json"),
            "{\"defaultProvider\":\"anthropic\",\"defaultModel\":\"private-model\"}",
        )
        .unwrap();
        let mut api = api();
        component.register(&mut api);
        let ctx = context(t.path(), "inventory");
        dispatch(&api, start(), &ctx).await;
        dispatch(&api, shutdown(), &ctx).await;
        let p = session_started(&messages);
        assert_eq!(p["default_model"], "custom");
        assert_eq!(p["default_provider"], "anthropic");
        assert_eq!(p["model_count"], 3);
        assert_eq!(p["provider_count"], 2);
        assert_eq!(p["providers"], "anthropic,custom");
        assert_eq!(p["configured_provider_count"], 2);
        assert_eq!(p["signed_in_provider_count"], 0);
        assert_eq!(p["connected_account_count"], "");
        assert_eq!(p["session_role"], "root");
        let mut expected: Vec<_> = crate::product_identity::EVENT_PROPERTY_ALLOWLISTS
            .iter()
            .find(|(n, _)| *n == "session_started")
            .unwrap()
            .1
            .iter()
            .copied()
            .filter(|key| *key != "timezone")
            .collect();
        expected.extend([
            // Common attribution keys from upstream event-schemas.ts (schema v4).
            "ci",
            "event_id",
            "install_hash",
            "machine_hash",
            "surface",
            "$process_person_profile",
            "package_version",
            "platform",
            "product_name",
            "schema_version",
        ]);
        expected.sort();
        let mut actual: Vec<_> = p.keys().map(String::as_str).collect();
        actual.sort();
        assert_eq!(actual, expected);
        assert!(!serde_json::to_string(&p).unwrap().contains("private"));
    }

    #[tokio::test]
    async fn linked_accounts_are_counted_not_named() {
        let t = tempfile::tempdir().unwrap();
        let (component, messages) = component(t.path());
        std::fs::write(
            t.path().join("auth.json"),
            serde_json::json!({
                "anthropic": {"key": "sk-secret"},
                "my-gateway": {"accounts": [{"name": "a"}, {"name": "b"}]}
            })
            .to_string(),
        )
        .unwrap();
        let mut api = api();
        component.register(&mut api);
        let ctx = context(t.path(), "accounts");
        dispatch(&api, start(), &ctx).await;
        dispatch(&api, shutdown(), &ctx).await;
        let p = session_started(&messages);
        assert_eq!(p["connected_account_count"], "anthropic:1,custom:2");
        assert_eq!(p["signed_in_provider_count"], 2);
        assert!(!serde_json::to_string(&p).unwrap().contains("sk-secret"));
    }

    #[tokio::test]
    async fn disabled_component_flag_registers_nothing() {
        let t = tempfile::tempdir().unwrap();
        let (component, messages) = component(t.path());
        let mut api = api();
        api.runtime.set_flag(
            "omo-senpi-telemetry-disabled",
            maho_ext_api::FlagValue::Boolean(true),
        );
        component.register(&mut api);
        dispatch(&api, start(), &context(t.path(), "disabled")).await;
        assert!(messages.lock().unwrap().is_empty());
        assert!(api.registered.handlers.is_empty());
    }

    #[tokio::test]
    async fn warmup_host_reports_nothing() {
        let t = tempfile::tempdir().unwrap();
        let (component, messages) = component(t.path());
        let mut api = api();
        api.profile.session_context.insert("host_warmup".into(), "1".into());
        component.register(&mut api);
        dispatch(&api, start(), &context(t.path(), "warmup")).await;
        assert!(messages.lock().unwrap().is_empty());
        assert!(api.registered.handlers.is_empty());
    }

    #[tokio::test]
    async fn injected_shutdown_bound() {
        let t = tempfile::tempdir().unwrap();
        let (mut component, _) = component(t.path());
        let delays = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&delays);
        component.options.set_timeout_fn = Some(Arc::new(move |delay| {
            recorded.lock().unwrap().push(delay);
            Box::pin(futures::future::pending())
        }));
        let mut api = api();
        component.register(&mut api);
        let ctx = context(t.path(), "s");
        dispatch(&api, start(), &ctx).await;
        dispatch(&api, shutdown(), &ctx).await;
        assert_eq!(
            *delays.lock().unwrap(),
            vec![std::time::Duration::from_millis(1000)]
        );
    }

    #[tokio::test]
    async fn native_feature_event_stream() {
        let t = tempfile::tempdir().unwrap();
        let (component, messages) = component(t.path());
        let mut api = api();
        component.register(&mut api);
        let ctx = context(t.path(), "stream");
        dispatch(&api, start(), &ctx).await;
        dispatch(
            &api,
            ExtensionEvent::ToolResult(maho_ext_api::ToolResultEvent {
                tool_call_id: "goal".into(),
                tool_name: "create_goal".into(),
                input: serde_json::json!({}),
                content: Vec::new(),
                details: None,
                is_error: false,
                usage: None,
            }),
            &ctx,
        )
        .await;
        dispatch(&api, shutdown(), &ctx).await;
        let messages = messages.lock().unwrap();
        let native: Vec<_> = messages
            .iter()
            .filter(|m| m.event != "omo_senpi_daily_active")
            .collect();
        assert_eq!(
            native.iter().map(|m| m.event.as_str()).collect::<Vec<_>>(),
            vec![
                "daily_active",
                "session_started",
                "category_config",
                "feature_used"
            ]
        );
        assert_eq!(native[3].properties["feature"], "goal_tool");
        assert!(!native[3].properties.contains_key("input"));
        assert!(!t.path().join("native/last-payloads.json").exists());
    }

    #[tokio::test]
    async fn daily_once_per_utc_day() {
        let t = tempfile::tempdir().unwrap();
        let (mut component, messages) = component(t.path());
        let day = chrono::DateTime::parse_from_rfc3339("2026-07-03T04:05:06Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        component.options.now = Some(day);
        for (id, now) in [
            ("first", day),
            ("stale", day),
            ("tomorrow", day + chrono::Duration::days(1)),
        ] {
            component.options.now = Some(now);
            let mut api = api();
            component.register(&mut api);
            let ctx = context(t.path(), id);
            dispatch(&api, start(), &ctx).await;
            dispatch(&api, shutdown(), &ctx).await;
        }
        let messages = messages.lock().unwrap();
        let days: Vec<_> = messages
            .iter()
            .filter(|m| m.event == "daily_active")
            .map(|m| m.properties["day_utc"].as_str().unwrap())
            .collect();
        assert_eq!(days, vec!["2026-07-03", "2026-07-04"]);
    }

    #[tokio::test]
    async fn optout_does_not_create_marker() {
        let t = tempfile::tempdir().unwrap();
        let (mut component, messages) = component(t.path());
        component
            .options
            .env
            .as_mut()
            .unwrap()
            .insert("DO_NOT_TRACK".into(), "1".into());
        let mut api = api();
        component.register(&mut api);
        dispatch(&api, start(), &context(t.path(), "s")).await;
        assert!(messages.lock().unwrap().is_empty());
        assert!(!t.path().join("native").exists());
    }

    #[tokio::test]
    async fn empty_inventory_valid_session() {
        let t = tempfile::tempdir().unwrap();
        let (component, messages) = component(t.path());
        let mut api = api();
        component.register(&mut api);
        let ctx = context(t.path(), "s");
        dispatch(&api, start(), &ctx).await;
        dispatch(&api, shutdown(), &ctx).await;
        let p = session_started(&messages);
        assert_eq!(p["provider_count"], 0);
        assert_eq!(p["model_count"], 0);
        assert_eq!(p["providers"], "");
        assert!(!p.contains_key("default_model"));
    }

    #[tokio::test]
    async fn shutdown_summary_precedes_client_teardown() {
        let t = tempfile::tempdir().unwrap();
        let (component, messages) = component(t.path());
        let mut api = api();
        component.register(&mut api);
        let ctx = context(t.path(), "ordering-session");
        dispatch(&api, start(), &ctx).await;
        for id in ["a", "b"] {
            dispatch(
                &api,
                ExtensionEvent::ToolExecutionStart {
                    tool_call_id: id.into(),
                    parent_tool_call_id: None,
                    tool_name: "bash".into(),
                    args: serde_json::json!({}),
                },
                &ctx,
            )
            .await;
        }
        for id in ["a", "b"] {
            dispatch(
                &api,
                ExtensionEvent::ToolExecutionEnd {
                    tool_call_id: id.into(),
                    parent_tool_call_id: None,
                    tool_name: "bash".into(),
                    result: serde_json::json!(1),
                    is_error: false,
                },
                &ctx,
            )
            .await;
        }
        dispatch(&api, shutdown(), &ctx).await;
        dispatch(&api, shutdown(), &ctx).await;
        let messages = messages.lock().unwrap();
        let summaries: Vec<_> = messages
            .iter()
            .filter(|m| m.event == "parallelism_summary")
            .collect();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].properties["non_eval_joined_calls"], 2);
    }

    #[tokio::test]
    async fn disabled_config_captures_nothing() {
        let t = tempfile::tempdir().unwrap();
        let (mut component, messages) = component(t.path());
        component.is_config_enabled = Arc::new(|_| false);
        component.options.is_config_enabled = Some(Arc::clone(&component.is_config_enabled));
        let mut api = api();
        component.register(&mut api);
        dispatch(&api, start(), &context(t.path(), "disabled")).await;
        assert!(messages.lock().unwrap().is_empty());
        assert!(!t.path().join("native/notice-shown").exists());
    }

    #[tokio::test]
    async fn restart_refreshes_session_client() {
        let t = tempfile::tempdir().unwrap();
        let (component, messages) = component(t.path());
        let mut api = api();
        component.register(&mut api);
        for id in ["first", "second"] {
            let ctx = context(t.path(), id);
            dispatch(&api, start(), &ctx).await;
            dispatch(&api, shutdown(), &ctx).await;
            dispatch(&api, shutdown(), &ctx).await;
        }
        let messages = messages.lock().unwrap();
        let sessions: Vec<_> = messages
            .iter()
            .filter(|m| m.event == "session_started")
            .collect();
        assert_eq!(sessions.len(), 2);
        assert_ne!(
            sessions[0].properties["$session_id"],
            sessions[1].properties["$session_id"]
        );
    }

    #[tokio::test]
    async fn opted_out_registration_writes_no_summary_state() {
        let t = tempfile::tempdir().unwrap();
        let (mut component, messages) = component(t.path());
        component.is_config_enabled = Arc::new(|_| false);
        component.options.is_config_enabled = Some(Arc::clone(&component.is_config_enabled));
        let mut api = api();
        component.register(&mut api);
        let ctx = context(t.path(), "opted-out");
        dispatch(&api, start(), &ctx).await;
        dispatch(
            &api,
            ExtensionEvent::TurnEnd {
                turn_index: 1,
                message: maho_ext_api::AgentMessage::Llm(maho_ai::types::Message::Assistant(Box::new(maho_ai::providers::faux::faux_assistant_message(
                    "hello",
                    Default::default(),
                )))),
                tool_results: Vec::new(),
            },
            &ctx,
        )
        .await;
        dispatch(&api, shutdown(), &ctx).await;
        assert!(messages.lock().unwrap().is_empty());
        assert!(!t.path().join("native/session-summary").exists());
        assert!(!t.path().join("native/owners").exists());
    }

    #[tokio::test]
    async fn a_turn_summary_is_published_before_the_client_shuts_down() {
        let t = tempfile::tempdir().unwrap();
        let (mut component, messages) = component(t.path());
        component.options.raw_turn_sample_rate = Some(1.0);
        let mut api = api();
        component.register(&mut api);
        let ctx = context(t.path(), "turns");
        dispatch(&api, start(), &ctx).await;
        dispatch(
            &api,
            ExtensionEvent::TurnStart {
                turn_index: 1,
                timestamp: 1,
            },
            &ctx,
        )
        .await;
        dispatch(
            &api,
            ExtensionEvent::TurnEnd {
                turn_index: 1,
                message: maho_ext_api::AgentMessage::Llm(maho_ai::types::Message::Assistant(Box::new(maho_ai::providers::faux::faux_assistant_message(
                    "hello",
                    Default::default(),
                )))),
                tool_results: Vec::new(),
            },
            &ctx,
        )
        .await;
        dispatch(&api, shutdown(), &ctx).await;
        let messages = messages.lock().unwrap();
        assert_eq!(
            messages
                .iter()
                .filter(|m| m.event == "turn_completed")
                .count(),
            1
        );
        let summary = messages
            .iter()
            .find(|m| m.event == "session_summary")
            .expect("session_summary");
        assert_eq!(summary.properties["ended_by"], "shutdown");
        assert_eq!(summary.properties["turns"], 1);
        assert_eq!(summary.properties["is_final"], true);
        assert_eq!(summary.properties["session_role"], "root");
        assert!(summary.uuid.is_some());
    }
}
