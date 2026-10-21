use crate::{
    index::SenpiTelemetryOptions, omo_native_consent::NativeConsent,
    omo_native_session_summary::SessionSummaryRecorder, session_summary_window::*,
};
use serde_json::json;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

#[test]
fn summary_quantiles_models_and_full_precision_cost() {
    let mut window = open_window(1000.0, &json!(1));
    for (index, total) in [(1, 10), (2, 20), (3, 90)] {
        let event = json!({"turnIndex":index,"message":{"role":"assistant","provider":"openai","model":"gpt-5.6-sol","stopReason":"error","usage":{"input":2,"output":3,"cacheRead":0,"cacheWrite":0,"totalTokens":total,"cost":{"total":0.000049}}},"toolResults":[{"isError":true}]});
        let (properties, invalid) =
            crate::omo_native_turns::turn_completed_properties(&event, "h").unwrap();
        add_turn(&mut window, &properties, &event, invalid, 2000.0);
    }
    let props = summary_properties("h", &window, 0, "shutdown", "main", 0.05);
    assert_eq!(props["turns"], 3);
    assert_eq!(props["total_tokens_p50"], 20.0);
    assert_eq!(props["total_tokens_p95"], 90.0);
    assert_eq!(props["cost_usd"], 0.0001);
    assert_eq!(props["tool_error_count"], 3.0);
    assert_eq!(props["error_turns"], 3.0);
    assert_eq!(props["m1_model"], "gpt-5.6-sol");
    assert_eq!(props["m2_model"], "none");
    assert!(props["is_final"].as_bool().unwrap());
    assert!(parse_window(&window).is_some());
}

#[test]
fn checkpoint_reload_and_opt_out_preserve_sequence_without_sending_revoked_window() {
    let dir = tempfile::tempdir().unwrap();
    let opted_in = Arc::new(AtomicBool::new(true));
    let enabled = Arc::clone(&opted_in);
    let consent = NativeConsent::new(
        &SenpiTelemetryOptions {
            env: Some(telemetry_core::TelemetryEnv::new()),
            ..Default::default()
        },
        Arc::new(move |_| enabled.load(Ordering::SeqCst)),
    );
    consent.set_cwd(dir.path());
    let rows = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&rows);
    let mut recorder = SessionSummaryRecorder::new(
        dir.path(),
        consent,
        Arc::new(move |name, props| captured.lock().unwrap().push((name.to_owned(), props))),
        Arc::new(|| 1000.0),
    );
    let hash = "a".repeat(64);
    let event = json!({"turnIndex":1,"message":{"role":"assistant","provider":"openai","model":"gpt-5.6-sol","usage":{"input":1,"output":1,"cacheRead":0,"cacheWrite":0,"totalTokens":2,"cost":{"total":0.1}}}});
    for _ in 0..50 {
        recorder.record(&hash, &event, dir.path(), "main").unwrap();
    }
    assert!(dir.path().join(format!("rollups/{hash}.json")).exists());
    recorder.record(&hash, &event, dir.path(), "main").unwrap();
    opted_in.store(false, Ordering::SeqCst);
    recorder.purge().unwrap();
    assert!(!dir.path().join(format!("rollups/{hash}.json")).exists());
    opted_in.store(true, Ordering::SeqCst);
    recorder.record(&hash, &event, dir.path(), "main").unwrap();
    recorder
        .finish(&hash, dir.path(), "reload", "main")
        .unwrap();
    recorder
        .finish(&hash, dir.path(), "shutdown", "main")
        .unwrap();
    let rows = rows.lock().unwrap();
    let summaries: Vec<_> = rows
        .iter()
        .filter(|(name, _)| name == "session_summary")
        .map(|(_, p)| p)
        .collect();
    assert_eq!(summaries.len(), 3);
    assert_eq!(summaries[0]["summary_seq"], 0);
    assert_eq!(summaries[0]["turns"], 50);
    assert_eq!(summaries[1]["summary_seq"], 1);
    assert_eq!(summaries[1]["ended_by"], "reload");
    assert_eq!(summaries[2]["summary_seq"], 2);
    assert_eq!(summaries[2]["turns"], 0);
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
    let options = SenpiTelemetryOptions {
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
        }),
        &ctx,
    )
    .await;
    let before = rows.lock().unwrap().len();
    opted_in.store(false, Ordering::SeqCst);
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

#[test]
fn orphan_window_is_claimed_once_and_uses_originating_consent() {
    let dir = tempfile::tempdir().unwrap();
    let options = SenpiTelemetryOptions {
        env: Some(telemetry_core::TelemetryEnv::new()),
        ..Default::default()
    };
    let consent = NativeConsent::new(&options, Arc::new(|cwd| !cwd.ends_with("revoked")));
    consent.set_cwd(dir.path());
    let store = crate::session_summary_store::SummaryStore::new(dir.path());
    for (hash, cwd) in [
        ("b".repeat(64), dir.path().to_owned()),
        ("c".repeat(64), dir.path().join("revoked")),
    ] {
        let mut value = crate::session_summary_store::rollup(
            &hash,
            2,
            open_window(1000.0, &json!(1)),
            &cwd,
            1000.0,
            "main",
        );
        value["pid"] = json!(2_147_483_647);
        store.write(&value).unwrap();
    }
    let rows = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&rows);
    let mut recorder = SessionSummaryRecorder::new(
        dir.path(),
        consent,
        Arc::new(move |name, props| captured.lock().unwrap().push((name.to_owned(), props))),
        Arc::new(|| 1001.0),
    );
    recorder
        .finish(&"d".repeat(64), dir.path(), "shutdown", "main")
        .unwrap();
    let rows = rows.lock().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].1["ended_by"], "recovered");
    assert_eq!(rows[0].1["summary_seq"], 2);
    assert!(
        !dir.path()
            .join(format!("rollups/{}.json", "c".repeat(64)))
            .exists()
    );
    assert!(store.claim_orphans(1002.0).unwrap().is_empty());
}
