use maho_server::app_server::turn_log::*;
use serde_json::json;
#[test]
fn record_append_complete_and_clone_isolation() {
    let mut log = TurnLog::default();
    let started = "2026-10-01T12:00:00.000Z";
    let turn = log.record_turn(
        "thread",
        RecordTurnOptions {
            turn_id: "turn".into(),
            started_at: started.into(),
            completed_at: None,
            error: None,
            status: None,
        },
    );
    assert_eq!(turn.status, TurnStatus::Running);
    assert_eq!(turn.duration_ms, None);
    log.append_item(
        "thread",
        "turn",
        json!({"id":"a","type":"agentMessage"})
            .as_object()
            .unwrap()
            .clone(),
    )
    .unwrap();
    let mut copy = log.read_turns("thread");
    copy[0].items.clear();
    assert_eq!(log.read_turns("thread")[0].items.len(), 1);
    log.complete_turn(
        "thread",
        "turn",
        CompleteTurnOptions {
            status: CompleteTurnStatus::Completed,
            completed_at: "2026-10-01T12:00:01.250Z".into(),
            error: None,
        },
    )
    .unwrap();
    let finished = log.read_turns("thread");
    assert_eq!(finished[0].duration_ms, Some(1250));
    assert_eq!(finished[0].status, TurnStatus::Completed);
    assert_eq!(
        log.append_item("missing", "turn", Default::default())
            .unwrap_err(),
        TurnNotFound("turn".into())
    );
    assert!(log.read_turns("missing").is_empty());
}
#[test]
fn duplicate_turn_ids_update_first_and_invalid_dates_have_null_duration() {
    let mut log = TurnLog::default();
    for _ in 0..2 {
        log.record_turn(
            "t",
            RecordTurnOptions {
                turn_id: "a".into(),
                started_at: "invalid".into(),
                completed_at: Some("invalid".into()),
                error: None,
                status: None,
            },
        );
    }
    log.complete_turn(
        "t",
        "a",
        CompleteTurnOptions {
            status: CompleteTurnStatus::Failed,
            completed_at: "invalid".into(),
            error: Some("failed".into()),
        },
    )
    .unwrap();
    let turns = log.read_turns("t");
    assert_eq!(turns[0].status, TurnStatus::Failed);
    assert_eq!(turns[1].status, TurnStatus::Running);
    assert_eq!(turns[0].duration_ms, None);
}
#[test]
fn duration_uses_non_rfc3339_date_parse() {
    let mut log = TurnLog::default();
    log.record_turn("t", RecordTurnOptions { turn_id: "a".into(), started_at: "2020-01-01T00:00:00.000Z".into(), completed_at: None, error: None, status: None });
    log.complete_turn("t", "a", CompleteTurnOptions { status: CompleteTurnStatus::Completed, completed_at: "2020-01-01 00:00:01".into(), error: None }).unwrap();
    assert_eq!(log.read_turns("t")[0].duration_ms, Some(1000));
}
