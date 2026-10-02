use maho_rpc::{loop_lag_watchdog::*, loop_blocked_time::LoopBlockedTime, session_attribution::*};

#[tokio::test(start_paused = true)]
async fn timer_publishes_attributed_stall_and_stops_when_cancelled() {
    let registry = SessionActivityRegistry::default();
    let _span = registry.open_span(SessionAttribution { session_id: Some("session".into()), tool: Some("bash".into()) }, None);
    let (published, mut samples) = tokio::sync::mpsc::unbounded_channel();
    let task = tokio::spawn(async move {
        let mut watchdog = LoopLagWatchdog::new(&Default::default());
        let mut blocked = LoopBlockedTime::default();
        let mut now = 0.;
        watchdog.run(&registry, &mut blocked, || { let value = now; now += 6000.; value }, |sample| { published.send(sample).unwrap(); }).await;
    });
    let sample = samples.recv().await.unwrap();
    assert_eq!(sample.record.unwrap()["sessionId"], "session");
    assert!(sample.log.is_some());
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
}
