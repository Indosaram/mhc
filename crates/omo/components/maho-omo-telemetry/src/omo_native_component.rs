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
    omo_native_session::{OPTED_OUT_SESSION_HASH, StartNativeSessionInput, start_native_session},
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
    /// The registration-time component logger (upstream `ctx.logger` of `register(pi, ctx)`); `None` writes
    /// registration diagnostics to stderr.
    pub logger: Option<Arc<dyn maho_ext_api::ComponentLogger>>,
    /// Terminal-edge ledger the task engine notifies. Injected in tests; defaults to the shared one.
    pub task_terminal_observers: Option<maho_omo_task::component::TaskTerminalObservers>,
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
            logger: None,
            task_terminal_observers: None,
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
        provider.zip(model).map(
            |(provider, model_id)| crate::omo_native_turn_span::TurnModel {
                provider: provider.to_string(),
                model_id: model_id.to_string(),
            },
        ),
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
            // The shared OTLP spool (beside the state dir) follows the same rule and is skipped while any
            // writer is alive. No spool on disk means nothing to judge.
            if persisted_opt_out && state_dir.join("..").join("otlp").exists() {
                crate::session_summary_recovery::purge_senpi_otlp_spool_on_opt_out(
                    &env,
                    self.options.is_pid_alive.clone(),
                    self.options.diagnostics.clone(),
                );
            }
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
                if properties
                    .get("$session_id")
                    .and_then(|value| value.as_str())
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
            let shutdown_env = env.clone();
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
                    let shutdown_env = shutdown_env.clone();
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
                                    state_dir: purge_state.clone(),
                                    is_scope_consented: Arc::new(move |scope| {
                                        consent.is_queue_scope_consented(scope)
                                    }),
                                    persisted_opt_out,
                                    is_pid_alive: options.is_pid_alive.clone(),
                                });
                                if persisted_opt_out
                                    && purge_state.join("..").join("otlp").exists()
                                {
                                    crate::session_summary_recovery::purge_senpi_otlp_spool_on_opt_out(
                                        &shutdown_env,
                                        options.is_pid_alive.clone(),
                                        options.diagnostics.clone(),
                                    );
                                }
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
                self.task_terminal_observers
                    .as_ref()
                    .unwrap_or_else(|| maho_omo_task::component::shared_task_terminal_observers()),
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
            let mut options = self.options.clone();
            // The one shared transport factory, built here so its consent is the live send-time gate.
            if options.transport_factory.is_none() {
                let send_consent = Arc::clone(&consent);
                options.transport_factory =
                    Some(crate::otlp::create_senpi_telemetry_transport_factory(
                        crate::otlp::TransportInput {
                            env: env.clone(),
                            consent: Arc::new(move || send_consent.is_consented(true)),
                            diagnostics: options.diagnostics.clone(),
                            source: Some("omo-native-component".into()),
                        },
                    ));
            }
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
            let session_closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let start_closed = Arc::clone(&session_closed);
            api.on(
                EventKind::SessionStart,
                Arc::new(move |event, ctx| {
                    consent.set_cwd(&ctx.cwd);
                    start_closed.store(false, std::sync::atomic::Ordering::SeqCst);
                    if let ExtensionEvent::SessionStart(event) = event {
                        let reason = match event.reason {
                            SessionReason::Startup => "startup",
                            SessionReason::Reload => "reload",
                            SessionReason::New => "new",
                            SessionReason::Resume => "resume",
                            SessionReason::Fork => "fork",
                            SessionReason::Quit => "startup",
                        };
                        // Reading the inventory, loading omo config and building the client is analytics on the
                        // session_start dispatch path that nothing in the session observes: it runs once the host's
                        // deferred tick fires (upstream `deferUntilAfterFirstPaint`), inline on a host without one.
                        // A shutdown that lands before the tick cancels the capture outright.
                        let defer = ctx.defer_macrotask.clone();
                        let ctx = ctx.clone();
                        let options = options.clone();
                        let consent = Arc::clone(&consent);
                        let hash = Arc::clone(&hash);
                        let session_capture = Arc::clone(&session_capture);
                        let categories = Arc::clone(&categories);
                        let summary = summary.clone();
                        let shared_client = Arc::clone(&shared_client);
                        let state = Arc::clone(&state);
                        let closed = Arc::clone(&start_closed);
                        let run = move || {
                            let ctx = &ctx;
                            if closed.load(std::sync::atomic::Ordering::SeqCst)
                                || !consent.is_consented(true)
                            {
                                return;
                            }
                            let payload = serde_json::json!({"reason": reason});
                            match start_native_session(&StartNativeSessionInput {
                                options: &options,
                                session_id: ctx.session_manager.session_id(),
                                payload: &payload,
                                agent_dir: &ctx.agent_dir,
                                session_role,
                                logger: ctx.logger.clone(),
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
                                                    Some(options) => facade.capture_immediate(
                                                        name,
                                                        &properties,
                                                        &options,
                                                    ),
                                                    None => {
                                                        facade.capture_event_with_options(
                                                            name,
                                                            &properties,
                                                            None,
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
                                    // The client teardown runs on another thread and may have landed after the
                                    // `closed` check above: re-check under the slot's lock (the teardown sets
                                    // `closed` before taking it), and shut a client built for a closed session
                                    // down here instead of leaving it installed and never shut down.
                                    {
                                        let mut slot = shared_client
                                            .lock()
                                            .unwrap_or_else(PoisonError::into_inner);
                                        if !closed.load(std::sync::atomic::Ordering::SeqCst) {
                                            *slot = session;
                                        } else {
                                            drop(slot);
                                            state.clear_capture();
                                            if let Some(session) = session {
                                                tokio::spawn(async move {
                                                    session.shutdown().await;
                                                });
                                            }
                                            return;
                                        }
                                    }
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
                                        &format!(
                                            "telemetry_capture_failed: omo-native-session: {error}"
                                        ),
                                    );
                                }
                            }
                        };
                        match defer {
                            Some(defer) => defer(Box::new(run)),
                            None => run(),
                        }
                    }
                    Box::pin(async { Ok(EventResult::None) })
                }),
            );

            // The session client's own teardown: clears the facade, then shuts the transport down.
            let state = shutdown_state;
            api.on(
                EventKind::SessionShutdown,
                Arc::new(move |_, _| {
                    session_closed.store(true, std::sync::atomic::Ordering::SeqCst);
                    let client = client.lock().unwrap_or_else(PoisonError::into_inner).take();
                    state.clear_capture();
                    Box::pin(async move {
                        if let Some(client) = client {
                            client.shutdown().await;
                        }
                        Ok(EventResult::None)
                    })
                }),
            );
        }

        {
            let turn_spans = Arc::clone(&turn_spans);
            let consent = Arc::clone(&consent);
            let hash = session_hasher(&consent, &state_dir);
            let state = Arc::clone(&state);
            api.on(
                EventKind::TurnStart,
                Arc::new(move |event, ctx| {
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
                }),
            );
        }

        {
            let turn_spans = Arc::clone(&turn_spans);
            let consent = Arc::clone(&consent);
            let summary = summary.clone();
            let diagnostics = self.options.diagnostics.clone();
            let hash = session_hasher(&consent, &state_dir);
            let session_capture = Arc::clone(&session_capture);
            api.on(
                EventKind::TurnEnd,
                Arc::new(move |event, ctx| {
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
                }),
            );
        }

        {
            let turn_spans = Arc::clone(&turn_spans);
            api.on(
                EventKind::AgentEnd,
                Arc::new(move |_, _| {
                    turn_spans.abandon();
                    Box::pin(async { Ok(EventResult::None) })
                }),
            );
        }

        // A valid OMO_BUNDLED_SKILLS_DIR names the tree the bundled-skills loader reads; telemetry follows it.
        // An invalid one is reported once per process to the component log (upstream
        // `getBuiltinSkillsRoot(env, ctx.logger)`).
        let logger = self.logger.clone();
        let skills_root =
            crate::product_identity::get_builtin_skills_root(&env, &self.skills_root, &|message| {
                match &logger {
                    Some(logger) => logger.error(message, None),
                    None => maho_ext_api::diagnostic(message),
                }
            });
        crate::omo_native_tools::register_omo_native_tool_telemetry(
            api,
            skills_root,
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
        ) -> Option<BoxFuture<'a, Result<(), telemetry_core::TelemetryDeliveryError>>> {
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
            sizes_only: false,
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
        api.profile
            .session_context
            .insert("host_warmup".into(), "1".into());
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
                message: maho_ext_api::AgentMessage::Llm(maho_ai::types::Message::Assistant(
                    std::sync::Arc::new(maho_ai::providers::faux::faux_assistant_message(
                        "hello",
                        Default::default(),
                    )),
                )),
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
                message: maho_ext_api::AgentMessage::Llm(maho_ai::types::Message::Assistant(
                    std::sync::Arc::new(maho_ai::providers::faux::faux_assistant_message(
                        "hello",
                        Default::default(),
                    )),
                )),
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

    fn turn_end() -> ExtensionEvent {
        ExtensionEvent::TurnEnd {
            turn_index: 1,
            message: maho_ext_api::AgentMessage::Llm(maho_ai::types::Message::Assistant(
                std::sync::Arc::new(maho_ai::providers::faux::faux_assistant_message(
                    "hello",
                    Default::default(),
                )),
            )),
            tool_results: Vec::new(),
        }
    }

    fn events(
        messages: &Arc<Mutex<Vec<TelemetryCaptureMessage>>>,
        name: &str,
    ) -> Vec<TelemetryCaptureMessage> {
        messages
            .lock()
            .unwrap()
            .iter()
            .filter(|m| m.event == name)
            .cloned()
            .collect()
    }

    fn task_edge(
        parent: &str,
        state_dir: &std::path::Path,
    ) -> maho_omo_task::component::TaskTerminalEdge {
        let mut record = senpi_task::state::create_task_record(
            senpi_task::state::TaskRecordInput {
                task_seq: Some(1),
                parent_session_id: parent.into(),
                root_session_id: parent.into(),
                execution_mode: "in-process".into(),
                model: "anthropic/claude-opus-5".into(),
                ..Default::default()
            },
            Some(0),
        )
        .expect("task record");
        record.status = senpi_task::state::TaskStatus::Completed;
        maho_omo_task::component::TaskTerminalEdge {
            record,
            previous_status: Some(senpi_task::state::TaskStatus::Running),
            state_dir: state_dir.to_path_buf(),
        }
    }

    struct Logger(Arc<Mutex<Vec<(&'static str, String)>>>);
    impl maho_ext_api::ComponentLogger for Logger {
        fn info(&self, message: &str, _: Option<&maho_ext_api::JsonValue>) {
            self.0.lock().unwrap().push(("info", message.into()));
        }
        fn warn(&self, message: &str, _: Option<&maho_ext_api::JsonValue>) {
            self.0.lock().unwrap().push(("warn", message.into()));
        }
        fn error(&self, message: &str, _: Option<&maho_ext_api::JsonValue>) {
            self.0.lock().unwrap().push(("error", message.into()));
        }
    }

    // omo v5.1.52 (#11626): the skills root resolves through `getBuiltinSkillsRoot(env, ctx.logger)`, so an
    // invalid OMO_BUNDLED_SKILLS_DIR reaches the component log, once per process.
    #[tokio::test]
    async fn an_invalid_skills_override_is_reported_once_to_the_component_logger() {
        let t = tempfile::tempdir().unwrap();
        let (mut component, _) = component(t.path());
        component.options.env.as_mut().unwrap().insert(
            "OMO_BUNDLED_SKILLS_DIR".into(),
            "relative/omo-native-component-logger".into(),
        );
        let entries = Arc::new(Mutex::new(Vec::new()));
        component.logger = Some(Arc::new(Logger(Arc::clone(&entries))));
        component.register(&mut api());
        component.register(&mut api());
        assert_eq!(
            *entries.lock().unwrap(),
            vec![(
                "error",
                "OMO_BUNDLED_SKILLS_DIR=relative/omo-native-component-logger ignored (relative); needs a readable absolute dir"
                    .to_string()
            )]
        );
    }

    #[tokio::test]
    async fn an_unsampled_session_sends_no_turn_completed_and_its_summary_carries_the_totals() {
        let t = tempfile::tempdir().unwrap();
        let (mut component, messages) = component(t.path());
        component.options.raw_turn_sample_rate = Some(0.0);
        let mut api = api();
        component.register(&mut api);
        let ctx = context(t.path(), "unsampled-session");
        dispatch(&api, start(), &ctx).await;
        dispatch(&api, turn_end(), &ctx).await;
        dispatch(&api, turn_end(), &ctx).await;
        dispatch(&api, shutdown(), &ctx).await;
        assert!(events(&messages, "turn_completed").is_empty());
        let summaries = events(&messages, "session_summary");
        assert_eq!(summaries.len(), 1);
        let p = &summaries[0].properties;
        assert_eq!(p["turns"], 2);
        assert_eq!(p["is_final"], true);
        assert_eq!(p["raw_turns_sampled"], false);
        assert_eq!(p["sample_rate"], 0.0);
    }

    #[tokio::test]
    async fn a_task_child_session_reports_its_summary_as_child() {
        let t = tempfile::tempdir().unwrap();
        let (mut component, messages) = component(t.path());
        component.options.raw_turn_sample_rate = Some(0.0);
        component
            .options
            .env
            .as_mut()
            .unwrap()
            .insert("OMO_SENPI_TASK_RPC_CHILD".into(), "1".into());
        let mut api = api();
        component.register(&mut api);
        let ctx = context(t.path(), "child-session");
        dispatch(&api, start(), &ctx).await;
        dispatch(&api, turn_end(), &ctx).await;
        dispatch(&api, shutdown(), &ctx).await;
        let summary = events(&messages, "session_summary");
        assert_eq!(summary[0].properties["session_role"], "child");
        assert_eq!(summary[0].properties["turns"], 1);
    }

    #[tokio::test]
    async fn a_foreign_delegation_hash_never_replaces_the_live_session_hash() {
        let t = tempfile::tempdir().unwrap();
        let (mut component, messages) = component(t.path());
        component.options.raw_turn_sample_rate = Some(1.0);
        let observers = maho_omo_task::component::TaskTerminalObservers::default();
        component.task_terminal_observers = Some(observers.clone());
        let mut api = api();
        component.register(&mut api);
        let ctx = context(t.path(), "live-session");
        dispatch(&api, start(), &ctx).await;
        // A terminal edge for a task spawned by an OLDER session.
        observers.notify(&task_edge("older-session", t.path()));
        dispatch(&api, turn_end(), &ctx).await;
        let native = t.path().join("native");
        let delegation = events(&messages, "delegation_completed");
        assert_eq!(delegation.len(), 1);
        assert_eq!(
            delegation[0].properties["$session_id"],
            crate::product_identity::hash_session_id("older-session", &native).unwrap()
        );
        assert_eq!(
            delegation[0].properties["schema_version"],
            crate::product_identity::OMO_NATIVE_SCHEMA_VERSION
        );
        assert_eq!(
            events(&messages, "turn_completed")[0].properties["$session_id"],
            crate::product_identity::hash_session_id("live-session", &native).unwrap()
        );
    }

    #[tokio::test]
    async fn a_terminal_edge_after_shutdown_captures_nothing() {
        let t = tempfile::tempdir().unwrap();
        let (mut component, messages) = component(t.path());
        let observers = maho_omo_task::component::TaskTerminalObservers::default();
        component.task_terminal_observers = Some(observers.clone());
        let mut api = api();
        component.register(&mut api);
        let ctx = context(t.path(), "disposed-session");
        dispatch(&api, start(), &ctx).await;
        dispatch(&api, shutdown(), &ctx).await;
        observers.notify(&task_edge("disposed-session", t.path()));
        assert!(events(&messages, "delegation_completed").is_empty());
    }

    #[tokio::test]
    async fn consent_is_read_for_the_session_cwd_before_every_turn() {
        let t = tempfile::tempdir().unwrap();
        let (mut component, _) = component(t.path());
        component.options.raw_turn_sample_rate = Some(0.0);
        // A frozen clock: only a fresh read can reach the config, never the 5 s cache expiring.
        component.options.consent_now = Some(Arc::new(|| 1_000.0));
        let reads = Arc::new(Mutex::new(Vec::<std::path::PathBuf>::new()));
        let observed = Arc::clone(&reads);
        component.is_config_enabled = Arc::new(move |cwd: &std::path::Path| {
            observed.lock().unwrap().push(cwd.to_path_buf());
            true
        });
        component.options.is_config_enabled = Some(Arc::clone(&component.is_config_enabled));
        let mut api = api();
        component.register(&mut api);
        let ctx = context(t.path(), "cached");
        dispatch(&api, start(), &ctx).await;
        let before = reads.lock().unwrap().len();
        for _ in 0..5 {
            dispatch(&api, turn_end(), &ctx).await;
        }
        let during: Vec<_> = reads.lock().unwrap()[before..].to_vec();
        assert!(during.len() >= 5, "{} reads", during.len());
        assert!(during.iter().all(|cwd| cwd == t.path()));
    }

    #[tokio::test]
    async fn restart_and_an_interrupted_prompt_keep_client_state_fresh() {
        let t = tempfile::tempdir().unwrap();
        let (component, messages) = component(t.path());
        let mut api = api();
        component.register(&mut api);
        let first = context(t.path(), "first");
        dispatch(&api, start(), &first).await;
        dispatch(&api, shutdown(), &first).await;
        let second = context(t.path(), "second");
        dispatch(&api, start(), &second).await;
        dispatch(
            &api,
            ExtensionEvent::Input(maho_ext_api::InputEvent {
                input_id: "interrupted".into(),
                text: "ordinary prompt".into(),
                images: None,
                source: maho_ext_api::InputSource::Interactive,
                streaming_behavior: None,
            }),
            &second,
        )
        .await;
        dispatch(&api, shutdown(), &second).await;
        dispatch(&api, shutdown(), &second).await;
        let native = t.path().join("native");
        let started: Vec<_> = events(&messages, "session_started")
            .into_iter()
            .map(|m| m.properties["$session_id"].clone())
            .collect();
        assert_eq!(
            started,
            vec![
                serde_json::json!(
                    crate::product_identity::hash_session_id("first", &native).unwrap()
                ),
                serde_json::json!(
                    crate::product_identity::hash_session_id("second", &native).unwrap()
                ),
            ]
        );
        assert_eq!(events(&messages, "prompt_submitted").len(), 1);
    }

    // #10226 / #10229: an env opt-out stops every producer before a session hash is derived, so no salt and
    // no summary window ever reach disk.
    #[tokio::test]
    async fn every_env_opt_out_stops_every_producer_and_writes_nothing() {
        for (name, value) in [
            ("DO_NOT_TRACK", "1"),
            ("OMO_DISABLE_TELEMETRY", "1"),
            ("OMO_DISABLE_POSTHOG", "1"),
            ("OMO_SENPI_SEND_ANONYMOUS_TELEMETRY", "0"),
        ] {
            let t = tempfile::tempdir().unwrap();
            let (mut component, messages) = component(t.path());
            component
                .options
                .env
                .as_mut()
                .unwrap()
                .insert(name.into(), value.into());
            let mut api = api();
            component.register(&mut api);
            let ctx = context(t.path(), "opted-out");
            dispatch(&api, start(), &ctx).await;
            dispatch(&api, turn_end(), &ctx).await;
            dispatch(&api, turn_end(), &ctx).await;
            dispatch(&api, shutdown(), &ctx).await;
            assert!(messages.lock().unwrap().is_empty(), "{name}");
            assert!(!t.path().join("native/session-id-salt").exists(), "{name}");
            assert!(!t.path().join("native/session-summary").exists(), "{name}");
        }
    }

    // Ported from omo-native-session.test.ts: the session half of the component lives in this registration.

    type DeferredQueue = Arc<Mutex<Vec<Box<dyn FnOnce() + Send>>>>;

    fn deferring_context(
        home: &std::path::Path,
        id: &str,
    ) -> (maho_ext_api::ExtensionContext, DeferredQueue) {
        let mut ctx = context(home, id);
        let queue: DeferredQueue = Arc::default();
        let tasks = Arc::clone(&queue);
        ctx.defer_macrotask = Some(Arc::new(move |task| tasks.lock().unwrap().push(task)));
        (ctx, queue)
    }

    #[tokio::test]
    async fn a_deferring_host_captures_nothing_on_the_dispatch_path_and_the_tick_captures_it() {
        let t = tempfile::tempdir().unwrap();
        let (component, messages) = component(t.path());
        let mut api = api();
        component.register(&mut api);
        let (ctx, queue) = deferring_context(t.path(), "deferred");
        dispatch(&api, start(), &ctx).await;
        assert!(messages.lock().unwrap().is_empty());
        let tasks = std::mem::take(&mut *queue.lock().unwrap());
        assert_eq!(tasks.len(), 1);
        for task in tasks {
            task();
        }
        assert_eq!(events(&messages, "session_started").len(), 1);
    }

    #[tokio::test]
    async fn a_shutdown_before_the_deferred_tick_creates_no_client() {
        let t = tempfile::tempdir().unwrap();
        let (mut component, messages) = component(t.path());
        let clients = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let created = Arc::clone(&clients);
        let captured = Arc::clone(&messages);
        component.options.transport_factory = Some(Arc::new(move |_, _| {
            created.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(Box::new(Recorder(Arc::clone(&captured))))
        }));
        let mut api = api();
        component.register(&mut api);
        let (ctx, queue) = deferring_context(t.path(), "closed");
        dispatch(&api, start(), &ctx).await;
        dispatch(&api, shutdown(), &ctx).await;
        for task in std::mem::take(&mut *queue.lock().unwrap()) {
            task();
        }
        assert!(messages.lock().unwrap().is_empty());
        assert_eq!(clients.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    struct SignalOnShutdown(Arc<tokio::sync::Notify>);
    impl TelemetryTransport for SignalOnShutdown {
        fn capture(&self, _: &TelemetryCaptureMessage) -> Result<(), TelemetryError> {
            Ok(())
        }
        fn flush(&self) -> Option<BoxFuture<'_, Result<(), TelemetryError>>> {
            None
        }
        fn shutdown(&self) -> BoxFuture<'_, Result<(), TelemetryError>> {
            self.0.notify_one();
            Box::pin(async { Ok(()) })
        }
    }

    #[tokio::test]
    async fn a_shutdown_landing_while_the_deferred_tick_builds_the_client_shuts_that_client_down() {
        let t = tempfile::tempdir().unwrap();
        let (mut component, _messages) = component(t.path());
        let shut_down = Arc::new(tokio::sync::Notify::new());
        // Armed after registration: the shutdown handlers and the context the factory fires them with.
        type ShutdownTrigger = (
            Vec<maho_ext_api::ExtensionHandler>,
            maho_ext_api::ExtensionContext,
        );
        let trigger: Arc<Mutex<Option<ShutdownTrigger>>> = Arc::default();
        let armed = Arc::clone(&trigger);
        let signal = Arc::clone(&shut_down);
        // The factory runs inside the tick after its `closed` check and before the client is stored: firing the
        // session shutdown here lands it exactly in that window.
        component.options.transport_factory = Some(Arc::new(move |_, _| {
            if let Some((handlers, ctx)) = armed.lock().unwrap().take() {
                let mut event = shutdown();
                for handler in &handlers {
                    drop(handler(&mut event, &ctx));
                }
            }
            Ok(Box::new(SignalOnShutdown(Arc::clone(&signal))))
        }));
        let mut api = api();
        component.register(&mut api);
        let (ctx, queue) = deferring_context(t.path(), "racing");
        *trigger.lock().unwrap() = Some((
            api.registered.handlers[&EventKind::SessionShutdown].clone(),
            ctx.clone(),
        ));
        dispatch(&api, start(), &ctx).await;
        for task in std::mem::take(&mut *queue.lock().unwrap()) {
            task();
        }
        assert!(trigger.lock().unwrap().is_none(), "the tick built a client");
        tokio::time::timeout(std::time::Duration::from_secs(5), shut_down.notified())
            .await
            .expect("the client built for the closed session is shut down");
    }

    #[test]
    fn an_unreadable_config_disables_telemetry_with_one_diagnostic() {
        let t = tempfile::tempdir().unwrap();
        // A directory where omo.json should be: it exists but cannot be read.
        std::fs::create_dir_all(t.path().join(".omo/omo.json")).unwrap();
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
        assert!(!(component.is_config_enabled)(t.path()));
        let diagnostics = diagnostics.lock().unwrap();
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(
            diagnostics[0].event,
            TelemetryDiagnosticEvent::TelemetryCaptureFailed
        );
        assert_eq!(
            diagnostics[0].error_kind,
            Some(TelemetryDiagnosticErrorKind::Error)
        );
        assert_eq!(diagnostics[0].source, "omo-native-session");
    }

    fn sha256_hex(value: &str) -> String {
        use sha2::Digest;
        format!("{:x}", sha2::Sha256::digest(value.as_bytes()))
    }

    /// A HOME of its own, so the install id never touches the real account; a temp HOME is an automated run
    /// unless it opts back in.
    fn with_private_home(component: &mut OmoNativeTelemetryComponent, home: &std::path::Path) {
        let env = component.options.env.as_mut().unwrap();
        env.insert("HOME".into(), home.to_string_lossy().into_owned());
        env.insert("USERPROFILE".into(), home.to_string_lossy().into_owned());
        env.insert("OMO_TELEMETRY_ALLOW_AUTOMATION".into(), "1".into());
    }

    #[tokio::test]
    async fn native_events_ship_under_the_hash_of_the_persisted_install_id() {
        let t = tempfile::tempdir().unwrap();
        let (mut component, messages) = component(t.path());
        with_private_home(&mut component, &t.path().join("home"));
        let mut api = api();
        component.register(&mut api);
        let ctx = context(t.path(), "identity");
        dispatch(&api, start(), &ctx).await;
        let path = crate::product_identity::get_omo_native_install_id_path(
            component.options.env.as_ref().unwrap(),
        );
        let install_id = std::fs::read_to_string(path).unwrap().trim().to_string();
        let install_hash = sha256_hex(&install_id);
        let messages = messages.lock().unwrap();
        let native: Vec<_> = messages
            .iter()
            .filter(|m| m.event != crate::index::SENPI_TELEMETRY_EVENT_NAME)
            .collect();
        assert!(!native.is_empty());
        for message in native {
            assert_eq!(message.distinct_id, install_hash);
            assert_eq!(message.properties["install_hash"], message.distinct_id);
            assert!(!message.properties.contains_key("install_id"));
            assert!(!message.properties.contains_key("machine_hash"));
            assert_eq!(message.properties["schema_version"], 4);
            assert!(
                !serde_json::to_string(&message.properties)
                    .unwrap()
                    .contains(&install_id)
            );
        }
    }

    #[tokio::test]
    async fn a_vanished_install_id_is_reminted_private_and_events_follow_it() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join("home");
        let (mut component, _) = component(t.path());
        with_private_home(&mut component, &home);
        let path = crate::product_identity::get_omo_native_install_id_path(
            component.options.env.as_ref().unwrap(),
        );
        let mut api = api();
        component.register(&mut api);
        dispatch(&api, start(), &context(t.path(), "before")).await;
        let first = std::fs::read_to_string(&path).unwrap().trim().to_string();
        std::fs::remove_file(&path).unwrap();
        let next = tempfile::tempdir().unwrap();
        let (mut later, messages) = self::component(next.path());
        with_private_home(&mut later, &home);
        let mut later_api = self::api();
        later.register(&mut later_api);
        dispatch(&later_api, start(), &context(next.path(), "after")).await;
        let second = std::fs::read_to_string(&path).unwrap().trim().to_string();
        assert_eq!(second.len(), 64);
        assert!(second.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(second, first);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert_eq!(
            events(&messages, "session_started")[0].distinct_id,
            sha256_hex(&second)
        );
    }

    #[tokio::test]
    async fn an_injected_time_zone_ships_and_category_config_follows_with_the_same_session() {
        let t = tempfile::tempdir().unwrap();
        let (mut component, messages) = component(t.path());
        component.options.time_zone = Some(Arc::new(|| "Asia/Seoul".into()));
        let mut api = api();
        component.register(&mut api);
        let ctx = context(t.path(), "zoned");
        let mut resume = start();
        if let ExtensionEvent::SessionStart(event) = &mut resume {
            event.reason = SessionReason::Resume;
        }
        dispatch(&api, resume, &ctx).await;
        let started = session_started(&messages);
        assert_eq!(started["timezone"], "Asia/Seoul");
        let configs = events(&messages, "category_config");
        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].properties["$session_id"], started["$session_id"]);
        assert_eq!(configs[0].properties["source"], "resume");
        let order: Vec<_> = messages
            .lock()
            .unwrap()
            .iter()
            .map(|m| m.event.clone())
            .collect();
        let position = |name: &str| order.iter().position(|event| event == name).unwrap();
        assert!(position("category_config") > position("session_started"));
    }

    #[tokio::test]
    async fn a_same_day_legacy_marker_stays_byte_identical_and_native_uses_its_own() {
        let t = tempfile::tempdir().unwrap();
        let (mut component, messages) = component(t.path());
        component.options.now = Some(
            chrono::DateTime::parse_from_rfc3339("2026-07-03T04:05:06Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        );
        let legacy_dir =
            crate::index::get_senpi_telemetry_state_dir(component.options.env.as_ref().unwrap());
        std::fs::create_dir_all(&legacy_dir).unwrap();
        let legacy_file = get_telemetry_activity_state_file_path(&legacy_dir);
        let before = b"{\"lastActiveDayUTC\":\"2026-07-03\"}\n".to_vec();
        std::fs::write(&legacy_file, &before).unwrap();
        let mut api = api();
        component.register(&mut api);
        let ctx = context(t.path(), "legacy-marker");
        dispatch(&api, start(), &ctx).await;
        dispatch(&api, shutdown(), &ctx).await;
        assert_eq!(std::fs::read(&legacy_file).unwrap(), before);
        assert_eq!(events(&messages, "daily_active").len(), 1);
        assert!(events(&messages, crate::index::SENPI_TELEMETRY_EVENT_NAME).is_empty());
    }
}
