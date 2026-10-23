use crate::{
    index::SenpiTelemetryOptions,
    omo_native_parallel::ParallelTelemetryRegistry,
    omo_native_parallel_summary::{SummaryCapture, register_omo_native_parallel_summary},
    product_identity::{get_omo_native_state_dir, hash_session_id},
};
use maho_ext_api::{
    BusSubscription, EventKind, EventResult, Extension, ExtensionApi, ExtensionEvent, SessionReason,
};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};
pub type ConfigEnabled = Arc<dyn Fn(&std::path::Path) -> bool + Send + Sync>;
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
        options: SenpiTelemetryOptions,
        skills_root: PathBuf,
        is_config_enabled: ConfigEnabled,
    ) -> Self {
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
                    });
                }
                return false;
            }
            omo_config_core::is_omo_telemetry_enabled(&loaded.config)
        });
        Self::new(options, skills_root, enabled)
    }
}
impl Extension for OmoNativeTelemetryComponent {
    fn register(&self, api: &mut ExtensionApi) {
        if api.runtime.get_flag("omo-senpi-telemetry-disabled")
            == Some(maho_ext_api::FlagValue::Boolean(true))
        {
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
        let notice_state_dir = state_dir.clone();
        let consent = crate::omo_native_consent::NativeConsent::new(
            &self.options,
            Arc::clone(&self.is_config_enabled),
        );
        consent.set_cwd(&api.cwd);
        let role = crate::omo_native_session_summary::session_role(&api.profile, &env);
        let shared: Arc<Mutex<Option<telemetry_core::EventTelemetryClient>>> = Arc::default();
        let client = Arc::clone(&shared);
        let capture_consent = Arc::clone(&consent);
        let capture: SummaryCapture = Arc::new(move |name, properties| {
            if !capture_consent.allowed() {
                return;
            }
            if let Some(client) = client
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref()
                && let Some(properties) = properties.as_object()
            {
                client.capture_event(name, properties);
            }
        });
        let summary = Arc::new(Mutex::new(
            crate::omo_native_session_summary::SessionSummaryRecorder::new(
                &state_dir,
                Arc::clone(&consent),
                Arc::clone(&capture),
                Arc::clone(&self.clock),
            ),
        ));
        let identity_client = Arc::clone(&shared);
        let hash: Arc<dyn Fn(&str) -> String + Send + Sync> = Arc::new(move |id| {
            if identity_client
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_none()
            {
                return String::new();
            }
            hash_session_id(id, &state_dir).unwrap_or_else(|error| {
                maho_ext_api::diagnostic(&format!("omo-native session identity failed: {error}"));
                String::new()
            })
        });
        let registry = Arc::new(Mutex::new(ParallelTelemetryRegistry::default()));
        let delegation_consent = Arc::clone(&consent);
        let delegation = Arc::new(Mutex::new(Some(
            crate::omo_native_delegation::create_omo_native_delegation_capture(
                maho_omo_task::component::shared_task_terminal_observers(),
                Arc::clone(&capture),
                Arc::clone(&hash),
                Arc::new(move || delegation_consent.allowed()),
                self.options.diagnostics.clone(),
            ),
        )));
        api.on(
            EventKind::SessionShutdown,
            Arc::new(move |_, _| {
                delegation
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take();
                Box::pin(async { Ok(EventResult::None) })
            }),
        );
        let now = Arc::clone(&self.clock);
        let subscription = register_omo_native_parallel_summary(
            api,
            registry,
            now,
            Arc::clone(&hash),
            Arc::clone(&capture),
        );
        self.subscriptions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(subscription);
        crate::omo_native_prompt::register_omo_native_prompt_telemetry(
            api,
            Arc::clone(&hash),
            Arc::clone(&capture),
        );
        let options = self.options.clone();
        let client = Arc::clone(&shared);
        let start_consent = Arc::clone(&consent);
        let start_capture = Arc::clone(&capture);
        let start_hash = Arc::clone(&hash);
        let categories = Arc::new(Mutex::new(
            crate::omo_native_category_config::CategoryConfigCapture::default(),
        ));
        api.on(EventKind::SessionStart,Arc::new(move |event,ctx| {start_consent.set_cwd(&ctx.cwd);let result=if start_consent.allowed() && let ExtensionEvent::SessionStart(event)=event {
            let reason=match event.reason {SessionReason::Startup=>"startup",SessionReason::Reload=>"reload",SessionReason::New=>"new",SessionReason::Resume=>"resume",SessionReason::Fork=>"fork",SessionReason::Quit=>"startup"};
            crate::omo_native_session::start_native_session(&options,ctx.session_manager.session_id(),&serde_json::json!({"reason":reason}),&ctx.agent_dir).map(|session|{
                *client.lock().unwrap_or_else(std::sync::PoisonError::into_inner)=session;
                let env=options.env.clone().unwrap_or_else(||std::env::vars().collect());
                match categories.lock().unwrap_or_else(std::sync::PoisonError::into_inner).observe(ctx,&env,&start_hash(ctx.session_manager.session_id()),reason) {
                    Ok(Some(properties))=>start_capture("category_config",properties),Ok(None)=>{},Err(error)=>warn(ctx,&format!("omo-native category config failed: {error}")),
                }
                if start_consent.allowed() {
                    let state=options.state_dir.clone().unwrap_or_else(||get_omo_native_state_dir(&env));
                    match crate::process_crash_records::claim_unreported_crash_records(&ctx.agent_dir,&state,options.now.unwrap_or_else(chrono::Utc::now)) {
                        Ok(records)=>for record in records {start_capture("process_crashed",crate::omo_native_crash::process_crashed_properties(&record,options.os_provider.as_deref().unwrap_or(telemetry_core::get_default_telemetry_os_provider())));},
                        Err(error)=>warn(ctx,&format!("omo-native crash capture failed: {error}")),
                    }
                }
            })
        } else {*client.lock().unwrap_or_else(std::sync::PoisonError::into_inner)=None;Ok(())};Box::pin(async move {if let Err(error)=result {warn(ctx,&format!("telemetry_capture_failed: omo-native-session: {error}"));}Ok(EventResult::None)})}));
        crate::omo_native_session_summary::register_session_summary(
            api,
            Arc::clone(&summary),
            Arc::clone(&hash),
            role,
        );
        api.on(
            EventKind::SessionShutdown,
            Arc::new(move |_, _| {
                let client = shared
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take();
                Box::pin(async move {
                    if let Some(client) = client {
                        client.shutdown().await;
                    }
                    Ok(EventResult::None)
                })
            }),
        );
        let turn_hash = Arc::clone(&hash);
        let diagnostics = self.options.diagnostics.clone();
        api.on(EventKind::TurnEnd,Arc::new(move |event,ctx| {
            if let ExtensionEvent::TurnEnd {turn_index,message,tool_results}=event {
                let event=serde_json::json!({"turnIndex":turn_index,"message":message,"toolResults":tool_results});
                let hash=turn_hash(ctx.session_manager.session_id());
                if let Some((_,true))=crate::omo_native_turns::turn_completed_properties(&event,&hash) && let Some(diagnostics)=&diagnostics {diagnostics(&telemetry_core::TelemetryDiagnosticInput {event:telemetry_core::TelemetryDiagnosticEvent::TelemetryEventPropertyRejected,source:"omo-native-turns".into(),error:Some(telemetry_core::TelemetryError::new("turn_end assistant usage contained missing or invalid values")),error_kind:Some(telemetry_core::TelemetryDiagnosticErrorKind::Error)});}
                if !hash.is_empty() && let Err(error)=summary.lock().unwrap_or_else(std::sync::PoisonError::into_inner).record(&hash,&event,&ctx.cwd,role) {warn(ctx,&format!("omo-native turn summary failed: {error}"));}
            }
            Box::pin(async {Ok(EventResult::None)})
        }));
        crate::omo_native_tools::register_omo_native_tool_telemetry(
            api,
            self.skills_root.clone(),
            hash,
            capture,
            Some(Arc::new(|ctx, message, details| {
                warn(ctx, &format!("{message}: {details}"))
            })),
        );
        crate::omo_native_notice::register_omo_native_notice(
            api,
            env,
            notice_state_dir,
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
    async fn public_session_shutdown_flushes_pending_prompt_before_disposal() {
        let t = tempfile::tempdir().unwrap();
        let (component, messages) = component(t.path());
        let provider = maho_ai::providers::faux::faux_provider(Default::default())
            .expect("valid faux provider");
        let model = provider.get_model(Some("faux-1")).unwrap();
        let cwd = t.path().to_string_lossy().into_owned();
        provider.set_responses(vec![
            maho_ai::providers::faux::faux_assistant_message(
                "Hello from the faux provider.",
                Default::default(),
            )
            .into(),
        ]);
        let runtime = maho_core::model_runtime::ModelRuntime::create_sync(
            maho_core::model_runtime::CreateModelRuntimeOptions {
                models_path: Some(t.path().join("models.json")),
                auth_path: Some(t.path().join("auth.json")),
                providers: Some(vec![provider.provider.clone()]),
                ..Default::default()
            },
        )
        .expect("valid model runtime");
        let session =
            maho_core::sdk::create_agent_session(maho_core::sdk::CreateAgentSessionOptions {
                cwd: Some(cwd.clone()),
                agent_dir: Some(cwd.clone()),
                model: Some(model),
                model_runtime: Some(runtime),
                session_manager: Some(maho_core::session_manager::SessionManager::in_memory(
                    &cwd, None, None,
                )),
                settings_manager: Some(maho_core::settings_manager::SettingsManager::from_storage(
                    Box::new(maho_core::settings_manager::InMemorySettingsStorage::default()),
                    false,
                )),
                auto_title_sessions: Some(false),
                ..Default::default()
            })
            .await
            .unwrap()
            .session;
        let loaded = maho_ext_host::loader::load_extensions(
            vec![maho_ext_host::loader::NativeExtensionFactory {
                path: "<todo-46-shutdown>".into(),
                source_info: maho_ext_api::SourceInfo::default(),
                extension: Box::new(component),
            }],
            t.path(),
            Default::default(),
        );
        let mut runner = maho_ext_host::ExtensionRunner::new(
            loaded.extensions,
            loaded.runtime,
            loaded.events,
            context(t.path(), &session.session_id()),
        );
        let input_runner = runner.clone();
        session.set_extension_runner(runner.clone()).await;
        session
            .bind_extensions(maho_core::agent_session::ExtensionBindings {
                mode: Some(maho_ext_api::ExtensionMode::Print),
                ..Default::default()
            })
            .await;
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            session.prompt("Say hello.", Default::default()),
        )
        .await
        .unwrap()
        .unwrap();
        // Current core emits Started disposition on initial prompt, so prompt_submitted
        // is captured immediately with turn_start metadata.
        assert_eq!(
            messages
                .lock()
                .unwrap()
                .iter()
                .filter(|m| m.event == "prompt_submitted")
                .count(),
            1
        );
        runner = input_runner;
        runner
            .emit(ExtensionEvent::Input(maho_ext_api::InputEvent {
                input_id: "pending-real-session".into(),
                text: "ulw resume later".into(),
                source: maho_ext_api::InputSource::Interactive,
                images: None,
                streaming_behavior: None,
            }))
            .await
            .unwrap();
        for id in ["overlap-a", "overlap-b"] {
            runner
                .emit(ExtensionEvent::ToolExecutionStart {
                    tool_call_id: id.into(),
                    parent_tool_call_id: None,
                    tool_name: "bash".into(),
                    args: serde_json::json!({}),
                })
                .await
                .unwrap();
        }
        for id in ["overlap-a", "overlap-b"] {
            runner
                .emit(ExtensionEvent::ToolExecutionEnd {
                    tool_call_id: id.into(),
                    parent_tool_call_id: None,
                    tool_name: "bash".into(),
                    result: serde_json::json!(1),
                    is_error: false,
                })
                .await
                .unwrap();
        }
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            session.emit_session_shutdown(SessionReason::Quit),
        )
        .await
        .unwrap();
        {
            let messages = messages.lock().unwrap();
            assert!(messages.iter().any(|m| m.event == "session_started"));
            let hash = hash_session_id(&session.session_id(), &t.path().join("native")).unwrap();
            assert_eq!(
                messages.iter().any(|m| m.event == "turn_completed"),
                crate::session_summary_window::is_session_sampled(
                    &hash,
                    crate::omo_native_session_summary::RAW_TURN_SAMPLE_RATE
                )
            );
            assert!(messages.iter().any(|m| m.event == "session_summary"));
            let prompts = messages
                .iter()
                .filter(|m| m.event == "prompt_submitted")
                .collect::<Vec<_>>();
            assert_eq!(prompts.len(), 2);
            assert_eq!(prompts[0].properties["queue_mode"], "immediate");
            assert_eq!(prompts[0].properties["is_turn_start"], true);
            assert_eq!(prompts[1].properties["queue_mode"], "other");
            assert_eq!(prompts[1].properties["is_turn_start"], false);
            assert_eq!(prompts[0].properties["keyword_variant"], "none");
            assert_eq!(prompts[1].properties["keyword_variant"], "ulw");
            assert_eq!(
                messages
                    .iter()
                    .filter(|m| m.event == "parallelism_summary")
                    .count(),
                1
            );
        }
        session.emit_session_shutdown(SessionReason::Quit).await;
        session.dispose().await;
        assert_eq!(
            messages
                .lock()
                .unwrap()
                .iter()
                .filter(|m| m.event == "prompt_submitted")
                .count(),
            2
        );
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
    #[tokio::test]
    async fn corrupt_inventory_reports_once_omits_defaults() {
        let t = tempfile::tempdir().unwrap();
        let (mut component, messages) = component(t.path());
        std::fs::write(t.path().join("models.json"), "{not json").unwrap();
        std::fs::write(
            t.path().join("settings.json"),
            "{\"defaultProvider\":\"anthropic\",\"defaultModel\":\"claude-sonnet-5\"}",
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
        let messages = messages.lock().unwrap();
        let p = &messages
            .iter()
            .find(|m| m.event == "session_started")
            .unwrap()
            .properties;
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
        let messages = messages.lock().unwrap();
        assert_eq!(
            messages
                .iter()
                .find(|m| m.event == "session_started")
                .unwrap()
                .properties["providers"],
            ""
        );
    }
    #[tokio::test]
    async fn session_inventory_privacy_exact_keys() {
        let t = tempfile::tempdir().unwrap();
        let (component, messages) = component(t.path());
        std::fs::write(t.path().join("models.json"),serde_json::json!({"providers":{"anthropic":{"models":[{"id":"claude-sonnet-5"},{"id":"private-model"}]},"private_cloud":{"models":[{"id":"secret-model"}]}}}).to_string()).unwrap();
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
        let messages = messages.lock().unwrap();
        let p = &messages
            .iter()
            .find(|m| m.event == "session_started")
            .unwrap()
            .properties;
        assert_eq!(p["default_model"], "custom");
        assert_eq!(p["default_provider"], "anthropic");
        assert_eq!(p["model_count"], 3);
        assert_eq!(p["provider_count"], 2);
        assert_eq!(p["providers"], "anthropic");
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
        assert!(!serde_json::to_string(p).unwrap().contains("private"));
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
        let messages = messages.lock().unwrap();
        let m = messages
            .iter()
            .find(|m| m.event == "session_started")
            .unwrap();
        assert_eq!(m.properties["provider_count"], 0);
        assert_eq!(m.properties["model_count"], 0);
        assert_eq!(m.properties["providers"], "");
        assert!(!m.properties.contains_key("default_model"));
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
}
