use maho_rpc::host_lifecycle::{HostActivity, ObservedHostTurns, wait_for_idle_exit};
use maho_rpc::observer_link::ObserverLink;

#[test]
fn observer_counts_runs_not_individual_loop_starts() {
    let mut turns = ObservedHostTurns::default();
    let mut observer = ObserverLink::default();
    observer.opened(1);
    assert!(!turns.observe("invalid"));
    assert!(!turns.observe(r#"{"type":"agent_start"}"#));
    for _ in 0..2 {
        assert!(turns.observe(r#"{"type":"agent_start","sessionId":"s"}"#));
    }
    assert_eq!(turns.activity(2, &observer, 0., 100.).active_turns, 1);
    assert!(turns.observe(r#"{"type":"agent_settled","sessionId":"s"}"#));
    assert_eq!(turns.busy_turns(), 0);
    assert_eq!(turns.activity(0, &observer, 0., 100.).active_turns, 0);
    // Duplicate terminal events are harmless, including after close.
    turns.observe(r#"{"type":"agent_settled","sessionId":"s"}"#);
    turns.observe(r#"{"type":"session_closed","sessionId":"s"}"#);
    assert_eq!(turns.busy_turns(), 0);
    observer.lost(1, false, 10.);
    assert_eq!(turns.activity(0, &observer, 109., 100.).active_turns, 1);
    assert_eq!(turns.activity(0, &observer, 110., 100.).active_turns, 0);
}

#[test]
fn closing_a_session_releases_only_its_run_even_without_settlement() {
    let mut turns = ObservedHostTurns::default();
    for session in ["a", "a", "b"] {
        turns.observe(&serde_json::json!({"type":"agent_start","sessionId":session}).to_string());
    }
    assert_eq!(turns.busy_turns(), 2);
    assert!(turns.observe(r#"{"type":"session_closed","sessionId":"a"}"#));
    assert_eq!(turns.busy_turns(), 1, "another session remains protected");
    turns.observe(r#"{"type":"agent_end","sessionId":"b"}"#);
    assert_eq!(
        turns.busy_turns(),
        1,
        "an iteration ending is not run settlement"
    );
    turns.observe(r#"{"type":"agent_settled","sessionId":"b"}"#);
    assert_eq!(turns.busy_turns(), 0);
    turns.observe(r#"{"type":"agent_start","sessionId":"a"}"#);
    assert_eq!(turns.busy_turns(), 1, "a later run is tracked normally");
}

#[tokio::test(start_paused = true)]
async fn a_settled_multi_iteration_run_no_longer_prevents_idle_exit() {
    let mut turns = ObservedHostTurns::default();
    turns.observe(r#"{"type":"agent_start","sessionId":"s"}"#);
    turns.observe(r#"{"type":"agent_start","sessionId":"s"}"#);
    turns.observe(r#"{"type":"agent_settled","sessionId":"s"}"#);
    let (_publisher, activity) = tokio::sync::watch::channel(HostActivity {
        connections: 0,
        active_turns: turns.busy_turns(),
    });
    tokio::time::timeout(
        std::time::Duration::from_millis(200),
        wait_for_idle_exit(100., activity),
    )
    .await
    .expect("settled run must not keep the supervisor alive")
    .expect("activity publisher remains open");
}

#[tokio::test]
async fn observer_reader_tracks_strict_jsonl_until_transport_eof() {
    use tokio::io::AsyncWriteExt;
    let (mut writer, reader) = tokio::io::duplex(1);
    let mut turns = ObservedHostTurns::default();
    let mut observer = ObserverLink::default();
    observer.opened(1);
    let mut activity = Vec::new();
    let read = turns.read_events(reader, |turns| {
        activity.push(turns.activity(0, &observer, 0., 100.).active_turns);
    });
    let write = async {
        writer
            .write_all(b"invalid\n{\"type\":\"agent_start\",\"sessionId\":\"s\"}\n{\"type\":\"agent_start\",\"sessionId\":\"s\"}\n{\"type\":\"session_closed\",\"sessionId\":\"s\"}")
            .await
            .unwrap();
        writer.shutdown().await.unwrap();
    };
    let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        tokio::join!(read, write)
    })
    .await
    .unwrap();
    result.unwrap();
    assert_eq!(activity, vec![1, 1, 0]);
}
