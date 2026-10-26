use crate::{
    index::SenpiTelemetryOptions,
    omo_native_session_summary::{
        SessionSummaryOptions, SessionSummaryRecorder, SessionSummaryRecorder as Recorder,
    },
    session_summary_publisher::SummaryPublish,
    session_summary_store::{
        ConsentScope, Probes, RecordConsent, WindowRecord, create_summary_storage,
    },
    session_summary_window::{
        EndedBy, SummaryMeta, SummaryTurn, add_turn, open_window, summary_properties, usd_of,
    },
};
use futures::future::BoxFuture;
use futures::FutureExt;
use serde_json::json;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

fn turn_event() -> serde_json::Value {
    json!({"turnIndex":1,"message":{"role":"assistant","provider":"openai","model":"gpt-5.6-sol","stopReason":"error","usage":{"input":2,"output":3,"cacheRead":0,"cacheWrite":0,"totalTokens":10,"cost":{"total":0.000049}}},"toolResults":[{"isError":true}]})
}

fn meta(ended_by: EndedBy, seq: u64) -> SummaryMeta {
    SummaryMeta {
        ended_by,
        epoch: "0123456789abcdef".into(),
        seq,
        sample_rate: 0.05,
        sampled: false,
        session_role: "root".into(),
        dropped_events: 0,
    }
}

#[test]
fn summary_quantiles_models_and_integer_cost() {
    let mut window = open_window(1000.0, 1.0);
    for (index, total) in [(1_u64, 10_u64), (2, 20), (3, 90)] {
        let mut event = turn_event();
        event["turnIndex"] = json!(index);
        event["message"]["usage"]["totalTokens"] = json!(total);
        let (_, invalid, turn) = crate::omo_native_turns::turn_completed_properties(
            &event,
            "h",
            0.05,
            crate::session_role::TelemetrySessionRole::Root,
        )
        .expect("assistant turn");
        add_turn(&mut window, &turn, 2000.0);
        assert!(!invalid);
    }
    let props = summary_properties("h", &window, &meta(EndedBy::Shutdown, 0));
    assert_eq!(props["turns"], 3);
    assert_eq!(props["total_tokens_p50"], 20.0);
    assert_eq!(props["total_tokens_p95"], 90.0);
    // 0.000049 rounds to zero integer units of 1e-4, so the derived USD figure is exactly zero.
    assert_eq!(props["cost_1e4"], 0.0);
    assert_eq!(props["cost_usd"], 0.0);
    assert_eq!(props["tool_error_count"], 3.0);
    assert_eq!(props["error_turns"], 3.0);
    assert_eq!(props["m1_model"], "gpt-5.6-sol");
    assert_eq!(props["m2_model"], "none");
    assert_eq!(props["is_final"], true);
    assert_eq!(props["session_role"], "root");
    assert_eq!(props["summary_epoch"], "0123456789abcdef");
}

#[test]
fn cost_is_rounded_once_and_usd_derives_from_the_integer() {
    for (cost, units) in [(0.00015, 2.0), (0.123456, 1235.0), (0.000049, 0.0)] {
        let mut event = turn_event();
        event["message"]["usage"]["cost"]["total"] = json!(cost);
        let (props, _, turn) = crate::omo_native_turns::turn_completed_properties(
            &event,
            "h",
            0.05,
            crate::session_role::TelemetrySessionRole::Root,
        )
        .expect("assistant turn");
        assert_eq!(props["cost_1e4"], units, "{cost}");
        assert_eq!(props["cost_usd"], usd_of(units));
        assert_eq!(turn.cost_1e4, units);
    }
}

fn recorder(
    dir: &std::path::Path,
    consented: Arc<AtomicBool>,
    rows: Arc<Mutex<Vec<serde_json::Value>>>,
) -> Arc<Recorder> {
    let publish: SummaryPublish = Arc::new(move |publication| {
        rows.lock().unwrap().push(serde_json::Value::Object(publication.properties));
        async { Ok(()) }.boxed() as BoxFuture<'static, _>
    });
    let storage = create_summary_storage(
        dir,
        crate::session_summary_owner::ProcessOwner {
            pid: std::process::id(),
            boot_id: "boot-self".into(),
        },
        Probes {
            is_pid_alive: Some(Arc::new(|_| false)),
            process_start_time: Some(Arc::new(|_| None)),
        },
        Some(Arc::new(|| 1000.0)),
    );
    SessionSummaryRecorder::new(SessionSummaryOptions {
        is_consented: Arc::new(move |_| consented.load(Ordering::SeqCst)),
        consent_scope: Arc::new(|| ConsentScope {
            cwd: Some("/repo".into()),
            config: None,
        }),
        is_scope_consented: Arc::new(|_| true),
        storage,
        publish,
        session_role: crate::session_role::TelemetrySessionRole::Root,
        sample_rate: Arc::new(|| 0.05),
        now: Some(Arc::new(|| 1000.0)),
        diagnostics: None,
        dropped_events: None,
        is_pid_alive: None,
    })
}

fn summary_turn(index: u64) -> SummaryTurn {
    SummaryTurn {
        turn_index: index as f64,
        provider: "openai".into(),
        model_id: "gpt-5.6-sol".into(),
        input_tokens: 1.0,
        output_tokens: 1.0,
        cache_read_tokens: 0.0,
        cache_write_tokens: 0.0,
        reasoning_tokens: 0.0,
        total_tokens: 2.0,
        cost_1e4: 1000.0,
        usage_valid: true,
        stop_reason: None,
        tool_result_count: 0.0,
        tool_error_count: 0.0,
    }
}

#[tokio::test]
async fn checkpoint_reload_and_opt_out_preserve_sequence_without_sending_revoked_window() {
    let dir = tempfile::tempdir().unwrap();
    let consented = Arc::new(AtomicBool::new(true));
    let rows = Arc::new(Mutex::new(Vec::new()));
    let recorder = recorder(dir.path(), Arc::clone(&consented), Arc::clone(&rows));
    let hash = "a".repeat(64);
    for index in 1..=50 {
        recorder.record_turn(&hash, summary_turn(index)).await;
    }
    assert!(dir
        .path()
        .join("session-summary")
        .join(format!("{hash}.json"))
        .exists());
    assert_eq!(rows.lock().unwrap().len(), 1);
    assert_eq!(rows.lock().unwrap()[0]["summary_seq"], 0);
    assert_eq!(rows.lock().unwrap()[0]["turns"], 50);

    // An opt-out mid-session deletes the window and sends nothing for it.
    consented.store(false, Ordering::SeqCst);
    recorder.purge_if_opted_out();
    assert!(!dir
        .path()
        .join("session-summary")
        .join(format!("{hash}.json"))
        .exists());
    consented.store(true, Ordering::SeqCst);

    recorder.record_turn(&hash, summary_turn(51)).await;
    recorder.finish(&hash, Some("reload")).await;
    recorder.record_turn(&hash, summary_turn(52)).await;
    recorder.finish(&hash, Some("shutdown")).await;
    let rows = rows.lock().unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[1]["ended_by"], "reload");
    // Upstream omo-native-session-summary.test.ts starts a new epoch after opt-out;
    // uniqueness is the (epoch, seq) pair, not a process-global sequence number.
    assert_eq!(rows[1]["summary_seq"], 0);
    assert_eq!(rows[2]["ended_by"], "shutdown");
    assert_eq!(rows[2]["summary_seq"], 1);
    assert_ne!(rows[0]["summary_epoch"], rows[1]["summary_epoch"]);
    assert_eq!(rows[1]["summary_epoch"], rows[2]["summary_epoch"]);
    assert_eq!(rows[2]["is_final"], true);
}

#[test]
fn crash_projection_rejects_private_versions_and_claims_once() {
    let dir = tempfile::tempdir().unwrap();
    let agent = dir.path().join("agent");
    let state = dir.path().join("state");
    std::fs::create_dir_all(agent.join("process-crashes")).unwrap();
    let now = chrono::Utc::now();
    let record = json!({"at":now.to_rfc3339(),"uptimeMs":1000,"kind":"private","detection":"unclean_exit","bunVersion":"/home/private","senpiVersion":"1.2.3","productVersion":"5.1.38"});
    std::fs::write(
        agent.join("process-crashes/crashes.jsonl"),
        record.to_string(),
    )
    .unwrap();
    let records =
        crate::process_crash_records::claim_unreported_crash_records(&agent, &state, now).unwrap();
    assert_eq!(records.len(), 1);
    assert!(
        crate::process_crash_records::claim_unreported_crash_records(&agent, &state, now)
            .unwrap()
            .is_empty()
    );
    let props = crate::omo_native_crash::process_crashed_properties(
        &records[0],
        telemetry_core::get_default_telemetry_os_provider(),
    );
    assert_eq!(props["crashed_bun_version"], "unknown");
    assert_eq!(props["process_kind"], "unknown");
    assert_eq!(props["signal"], "unknown");
    assert_eq!(props["uptime_bucket"], "lt_1m");
    assert!(!props.to_string().contains("private"));
}

#[tokio::test]
async fn live_opt_out_gates_feature_and_summary_emissions() {
    use crate::telemetry_test_support::{api, context, dispatch, env};
    use maho_ext_api::{
        Extension, ExtensionEvent, SessionReason, SessionShutdownEvent, SessionStartEvent,
        ToolResultEvent,
    };
    use telemetry_core::{TelemetryCaptureMessage, TelemetryError, TelemetryTransport};
    struct Capture(Arc<Mutex<Vec<TelemetryCaptureMessage>>>);
    impl TelemetryTransport for Capture {
        fn capture(&self, message: &TelemetryCaptureMessage) -> Result<(), TelemetryError> {
            self.0.lock().unwrap().push(message.clone());
            Ok(())
        }
        fn flush(&self) -> Option<futures::future::BoxFuture<'_, Result<(), TelemetryError>>> {
            None
        }
        fn shutdown(&self) -> futures::future::BoxFuture<'_, Result<(), TelemetryError>> {
            Box::pin(async { Ok(()) })
        }
    }
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("models.json"), "{\"providers\":{}}").unwrap();
    std::fs::write(dir.path().join("settings.json"), "{}").unwrap();
    let rows = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&rows);
    let opted_in = Arc::new(AtomicBool::new(true));
    let enabled = Arc::clone(&opted_in);
    let consent_clock = Arc::new(std::sync::atomic::AtomicU64::new(10_000));
    let read_clock = Arc::clone(&consent_clock);
    let options = SenpiTelemetryOptions {
        consent_now: Some(Arc::new(move || read_clock.load(Ordering::SeqCst) as f64)),
        env: Some(env(dir.path())),
        state_dir: Some(dir.path().join("native")),
        transport_factory: Some(Arc::new(move |_, _| {
            Ok(Box::new(Capture(Arc::clone(&captured))))
        })),
        ..Default::default()
    };
    let component = crate::omo_native_component::OmoNativeTelemetryComponent::new(
        options,
        dir.path().join("skills"),
        Arc::new(move |_| enabled.load(Ordering::SeqCst)),
    );
    let mut api = api();
    component.register(&mut api);
    let ctx = context(dir.path(), "consent-test");
    dispatch(
        &api,
        ExtensionEvent::SessionStart(SessionStartEvent {
            reason: SessionReason::Startup,
            initial_model_provenance: None,
            previous_session_file: None,
            attended: None,
        }),
        &ctx,
    )
    .await;
    let before = rows.lock().unwrap().len();
    opted_in.store(false, Ordering::SeqCst);
    // Upstream omo-native-component.test.ts advances the injected clock past the consent cache.
    consent_clock.fetch_add(crate::omo_native_consent::CONSENT_CACHE_MS as u64 + 1, Ordering::SeqCst);
    dispatch(
        &api,
        ExtensionEvent::ToolResult(ToolResultEvent {
            tool_call_id: "goal".into(),
            tool_name: "create_goal".into(),
            input: json!({}),
            content: Vec::new(),
            details: None,
            is_error: false,
            usage: None,
        }),
        &ctx,
    )
    .await;
    dispatch(
        &api,
        ExtensionEvent::SessionShutdown(SessionShutdownEvent {
            reason: SessionReason::Quit,
            target_session_file: None,
            signal: None,
        }),
        &ctx,
    )
    .await;
    assert_eq!(rows.lock().unwrap().len(), before);
}

#[tokio::test]
async fn orphan_window_is_claimed_once_and_uses_originating_consent() {
    let dir = tempfile::tempdir().unwrap();
    let rows: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
    let published = Arc::clone(&rows);
    let publish: SummaryPublish = Arc::new(move |publication| {
        published
            .lock()
            .unwrap()
            .push(serde_json::Value::Object(publication.properties));
        async { Ok(()) }.boxed() as BoxFuture<'static, _>
    });
    let storage = create_summary_storage(
        dir.path(),
        crate::session_summary_owner::ProcessOwner {
            pid: std::process::id(),
            boot_id: "boot-self".into(),
        },
        Probes {
            is_pid_alive: Some(Arc::new(|_| false)),
            process_start_time: Some(Arc::new(|_| None)),
        },
        Some(Arc::new(|| 1001.0)),
    );
    for (hash, cwd) in [
        ("b".repeat(64), "/repo".to_string()),
        ("c".repeat(64), "/revoked".to_string()),
    ] {
        storage
            .write_window(
                &hash,
                &WindowRecord {
                    consent: RecordConsent::Enabled,
                    scope: Some(ConsentScope {
                        cwd: Some(cwd),
                        config: None,
                    }),
                    session_hash: hash.clone(),
                    session_role: crate::session_role::TelemetrySessionRole::Root,
                    owner: crate::session_summary_owner::ProcessOwner {
                        pid: 2_147_483_647,
                        boot_id: "boot-dead".into(),
                    },
                    heartbeat_at: -1_000_000.0,
                    epoch: Some("0123456789abcdef".into()),
                    seq: 2,
                    sample_rate: 0.05,
                    window_id: Some("01234567-89ab-cdef-0123-456789abcdef".into()),
                    window: Some(open_window(1000.0, 1.0)),
                },
            )
            .unwrap();
    }
    let recorder = SessionSummaryRecorder::new(SessionSummaryOptions {
        is_consented: Arc::new(|_| true),
        consent_scope: Arc::new(|| ConsentScope {
            cwd: Some("/repo".into()),
            config: None,
        }),
        is_scope_consented: Arc::new(|scope| scope.cwd.as_deref() != Some("/revoked")),
        storage,
        publish,
        session_role: crate::session_role::TelemetrySessionRole::Root,
        sample_rate: Arc::new(|| 0.05),
        now: Some(Arc::new(|| 1001.0)),
        diagnostics: None,
        dropped_events: None,
        is_pid_alive: None,
    });
    recorder.recover().await;
    let rows = rows.lock().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["ended_by"], "recovered");
    assert_eq!(rows[0]["summary_seq"], 2);
    // The revoked project's window is dropped unsent, not recovered.
    assert!(
        !dir.path()
            .join("session-summary")
            .join(format!("{}.json", "c".repeat(64)))
            .exists()
    );
}
