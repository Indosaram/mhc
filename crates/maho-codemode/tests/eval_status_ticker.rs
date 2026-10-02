use std::sync::{Arc, atomic::{AtomicU64, Ordering}};
use maho_codemode::{extension::eval_status_ticker::*, tool::{detached_cell_contract::EvalDetachedCellStatusEntry, types::EvalLanguage}};

fn entry(started: f64) -> EvalDetachedCellStatusEntry {
    EvalDetachedCellStatusEntry { cell_id: "cell-1".into(), language: EvalLanguage::Py, summary: Some("long running cell".into()), started_at_ms: started, queued_behind: None }
}

fn fixture() -> (EvalStatusTicker, tokio::sync::mpsc::UnboundedReceiver<Option<String>>, Arc<AtomicU64>) {
    let (send, receive) = tokio::sync::mpsc::unbounded_channel();
    let clock = Arc::new(AtomicU64::new(1_000_000));
    let now = Arc::clone(&clock);
    let ticker = EvalStatusTicker::new(Arc::new(move |value| { send.send(value).expect("receiver alive"); }), Arc::new(move || now.load(Ordering::SeqCst) as f64));
    (ticker, receive, clock)
}

#[tokio::test(start_paused = true)]
async fn interval_lifetime_owned_by_ticker() {
    assert_eq!(EVAL_STATUS_TICK_INTERVAL_MS, 1000);
    let (mut ticker, mut receive, _) = fixture();
    ticker.sync(vec![entry(1_000_000.0)]);
    assert!(ticker.running());
    assert!(receive.recv().await.unwrap().is_some());
    drop(ticker);
    tokio::task::yield_now().await;
    assert!(receive.recv().await.is_none());
}

#[tokio::test(start_paused = true)]
async fn immediate_render_and_one_second_refresh() {
    let (mut ticker, mut receive, clock) = fixture();
    ticker.sync(vec![entry(1_000_000.0)]);
    assert_eq!(receive.recv().await.unwrap().unwrap(), "↗ py · long running cell (0s)");
    tokio::task::yield_now().await;
    clock.store(1_001_000, Ordering::SeqCst);
    tokio::time::advance(std::time::Duration::from_secs(1)).await;
    assert_eq!(receive.recv().await.unwrap().unwrap(), "↗ py · long running cell (1s)");
}

#[tokio::test(start_paused = true)]
async fn unchanged_label_suppresses_refresh() {
    let (mut ticker, mut receive, clock) = fixture();
    ticker.sync(vec![entry(940_000.0)]);
    assert!(receive.recv().await.unwrap().unwrap().ends_with("(1m)"));
    tokio::task::yield_now().await;
    clock.store(1_059_000, Ordering::SeqCst);
    tokio::time::advance(std::time::Duration::from_secs(59)).await;
    tokio::task::yield_now().await;
    assert!(receive.try_recv().is_err());
    clock.store(1_060_000, Ordering::SeqCst);
    tokio::time::advance(std::time::Duration::from_secs(1)).await;
    assert!(receive.recv().await.unwrap().unwrap().ends_with("(2m)"));
}

#[tokio::test(start_paused = true)]
async fn settlement_clears_and_stops_interval() {
    let (mut ticker, mut receive, _) = fixture();
    ticker.sync(vec![entry(1_000_000.0)]);
    assert!(receive.recv().await.unwrap().is_some());
    ticker.sync(vec![]);
    assert_eq!(receive.recv().await.unwrap(), None);
    assert!(!ticker.running());
    tokio::time::advance(std::time::Duration::from_secs(5)).await;
    assert!(receive.try_recv().is_err());
}

#[tokio::test(start_paused = true)]
async fn explicit_stop_drops_entries_without_rendering() {
    let (mut ticker, mut receive, _) = fixture();
    ticker.sync(vec![entry(1_000_000.0)]);
    assert!(receive.recv().await.unwrap().is_some());
    ticker.stop();
    assert!(!ticker.running());
    tokio::time::advance(std::time::Duration::from_secs(5)).await;
    assert!(receive.try_recv().is_err());
}
