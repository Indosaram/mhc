use super::*;
use crate::state::{
    CostReportStatus, TaskRecordInput, TaskRunStats, TaskStatus, create_task_record,
};
use serde_json::{Value, json};

fn fixture() -> crate::state::TaskRecord {
    create_task_record(
        TaskRecordInput {
            parent_session_id: "p".into(),
            root_session_id: "p".into(),
            depth: 1,
            execution_mode: "in-process".into(),
            model: "test/model".into(),
            ..Default::default()
        },
        Some(0),
    )
    .expect("record")
}

fn parse(value: &Value) -> Result<crate::state::TaskRecord, String> {
    parse_task_record(value, "record.json", &mut Vec::new())
}

#[test]
fn requested_and_ordered_fallback_models_survive_store_round_trip() {
    let root = tempfile::tempdir().expect("root");
    let config = StateDirConfig {
        project_dir: root.path().into(),
        task_state_dir: None,
    };
    let store = TaskRecordStore::new(&config);
    let mut record = fixture();
    let requested = crate::state::ResolvedModelRecord::new(
        crate::state::ResolvedModelSource::Category,
        "kimi-coding",
        "kimi-for-coding-highspeed-unlocked",
    );
    let fallback = crate::state::ResolvedModelRecord::new(
        crate::state::ResolvedModelSource::Category,
        "chatgpt-subscription",
        "gpt-5.6-luna-fast",
    );
    record.requested_model = Some(requested.clone());
    record.resolved_model = Some(requested);
    record.fallback_models = Some(vec![fallback]);
    store.save(&record).expect("save");
    let listing = TaskRecordStore::new(&config).list().expect("list");
    assert!(listing.diagnostics.is_empty());
    assert_eq!(listing.records[0].requested_model, record.requested_model);
    assert_eq!(listing.records[0].fallback_models, record.fallback_models);
}

#[test]
fn paused_status_uses_r0_encoding_and_restores_owner() {
    let root = tempfile::tempdir().expect("root");
    let store = TaskRecordStore::new(&StateDirConfig {
        project_dir: root.path().into(),
        task_state_dir: None,
    });
    let mut raw = serde_json::to_value(fixture()).expect("json");
    raw["status"] = json!("paused");
    raw["run_pause"] = json!({"parent_session_id":"parent-1", "parent_pause_seq":2, "run_ref":{"kind":"task", "id":"st_0000a0c1"}});
    let record = parse(&raw).expect("paused");
    store.save(&record).expect("save");
    let path = store
        .state_dir()
        .join("tasks")
        .join(format!("{}.json", record.task_id));
    let stored: Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("read")).expect("json");
    assert_eq!(stored["status"], "running");
    assert_eq!(stored["paused"], true);
    let loaded = store.load(&record.task_id).expect("load").expect("record");
    assert_eq!(loaded.status, TaskStatus::Paused);
    assert_eq!(loaded.run_pause, record.run_pause);
    let mut running = record;
    running.status = TaskStatus::Running;
    store.replace(&running).expect("replace");
    assert!(
        to_storable_task_record(&running)
            .expect("stored")
            .get("paused")
            .is_none()
    );
    assert_eq!(
        store
            .load(&running.task_id)
            .expect("load")
            .expect("record")
            .status,
        TaskStatus::Running
    );
}

#[test]
fn resident_figures_count_saved_bytes_and_forget_removed_records() {
    let root = tempfile::tempdir().expect("root");
    let store = TaskRecordStore::new(&StateDirConfig {
        project_dir: root.path().into(),
        task_state_dir: None,
    });
    let mut first = fixture();
    first.task_id = "st_00000001".into();
    let mut second = fixture();
    second.task_id = "st_00000002".into();
    store.save(&first).expect("save");
    store.save(&second).expect("save");
    let bytes = [&first, &second]
        .iter()
        .map(|record| {
            std::fs::metadata(
                store
                    .state_dir()
                    .join("tasks")
                    .join(format!("{}.json", record.task_id)),
            )
            .expect("metadata")
            .len() as usize
        })
        .sum();
    assert_eq!(
        store.resident_figures(),
        TaskRecordStoreResidentFigures {
            cached_records: 2,
            cached_record_bytes: bytes,
            open_event_logs: 0
        }
    );
    store.remove(&first.task_id).expect("remove");
    store.remove(&second.task_id).expect("remove");
    assert_eq!(
        store.resident_figures(),
        TaskRecordStoreResidentFigures {
            cached_records: 0,
            cached_record_bytes: 0,
            open_event_logs: 0
        }
    );
}

#[test]
fn rollback_preserves_partial_cost_and_normalizes_only_for_r0() {
    let root = tempfile::tempdir().expect("root");
    let store = TaskRecordStore::new(&StateDirConfig {
        project_dir: root.path().into(),
        task_state_dir: Some(root.path().into()),
    });
    let mut record = fixture();
    record.status = TaskStatus::Completed;
    record.run_stats = Some(TaskRunStats {
        runtime_ms: 1,
        turns: 2,
        tool_calls: 0,
        cost_usd: Some(0.5),
        cost_status: Some(CostReportStatus::Partial),
        ..Default::default()
    });
    store.save(&record).expect("save");
    assert_eq!(
        store
            .load(&record.task_id)
            .expect("load")
            .expect("record")
            .run_stats,
        record.run_stats
    );
    let result = rollback_migrate::migrate_host_session_sockets(
        root.path(),
        "/tmp/rpc.sock",
        &Default::default(),
        false,
    )
    .expect("migration");
    assert_eq!(result.migrated, 1);
    let loaded = store.load(&record.task_id).expect("load").expect("record");
    let stats = loaded.run_stats.expect("stats");
    assert_eq!(stats.cost_status, Some(CostReportStatus::Reported));
    assert_eq!(stats.cost_usd, Some(0.5));
    assert_eq!(
        rollback_migrate::plan_host_session_socket_migration(root.path(), "/tmp/rpc.sock")
            .expect("plan")
            .migrate,
        0
    );
}

#[test]
fn parent_fallback_provenance_drops_malformed_or_future_values() {
    let mut value = serde_json::to_value(fixture()).expect("json");
    let valid = json!({"from_model":"provider/model", "reason":"usage_limit", "at":"now", "detail":"limit", "resets":"later", "interrupted_tools":["read"]});
    value["parent_model_fallback"] = valid.clone();
    assert_eq!(
        serde_json::to_value(
            parse(&value)
                .expect("record")
                .parent_model_fallback
                .expect("fallback")
        )
        .expect("json"),
        valid
    );
    for invalid in [
        Value::Null,
        json!(7),
        json!({}),
        json!({"from_model":"a", "at":"now", "reason":"future"}),
        json!({"from_model":"a", "at":"now", "reason":"error", "detail":null}),
    ] {
        value["parent_model_fallback"] = invalid;
        assert!(
            parse(&value)
                .expect("record retained")
                .parent_model_fallback
                .is_none()
        );
    }
    value["parent_model_fallback"] =
        json!({"from_model":"a", "reason":"error", "at":"now", "interrupted_tools":[]});
    assert!(
        parse(&value)
            .expect("record")
            .parent_model_fallback
            .expect("fallback")
            .interrupted_tools
            .is_none()
    );
}

#[test]
fn optional_lifecycle_fields_round_trip_and_reject_malformed_blocks() {
    let mut value = serde_json::to_value(fixture()).expect("json");
    let fields = json!({
        "fallback_closing_child":{"pid":42, "requires_confirmation":true},
        "start_queued":{"model":"test/model", "queued_at":"now", "queue_position":2},
        "revive_delivery_uncertain":{"run_epoch":3, "message_sha256":"hash"},
        "awaiting_input":{"reason":"resumed_turn_aborted", "since":"now"},
        "cancel_requested":{"requested_at":"now", "reason":"stop"},
        "prior_runs_cost_usd":0.75, "background_mode":"promoted",
        "resumed_run_epoch":3, "burnt_epoch":4, "fallback_handoff_epoch":5,
        "revival_deferred_reason":"capacity", "recovery_deadline_at":100, "residency_claim":"owner"
    });
    for (key, field) in fields.as_object().expect("fields") {
        value[key] = field.clone();
    }
    let parsed = parse(&value).expect("record");
    let serialized = serde_json::to_value(&parsed).expect("json");
    assert_eq!(parse(&serialized).expect("round trip"), parsed);
    assert!(parsed.fallback_closing_child.is_some());
    assert!(parsed.start_queued.is_some());
    assert!(parsed.revive_delivery_uncertain.is_some());
    assert_eq!(parsed.prior_runs_cost_usd, Some(0.75));
    for key in [
        "fallback_closing_child",
        "start_queued",
        "revive_delivery_uncertain",
        "awaiting_input",
        "cancel_requested",
    ] {
        let mut malformed = value.clone();
        malformed[key] = json!(false);
        assert!(parse(&malformed).is_err(), "{key}");
    }
    value["prior_runs_cost_usd"] = json!(-0.5);
    assert!(parse(&value).is_err());
    value["prior_runs_cost_usd"] = json!(0);
    assert!(parse(&value).is_ok());
}

#[test]
fn run_pause_validates_owner_sequence_and_run_reference() {
    let mut value = serde_json::to_value(fixture()).expect("json");
    for kind in ["dag", "task", "workpool", "team"] {
        value["run_pause"] = json!({"parent_session_id":"p", "parent_pause_seq":1.0, "run_ref":{"kind":kind, "id":"run"}});
        assert!(parse(&value).expect("record").run_pause.is_some());
    }
    for pause in [
        json!(null),
        json!({}),
        json!({"parent_session_id":"", "parent_pause_seq":1, "run_ref":{"kind":"task", "id":"x"}}),
        json!({"parent_session_id":"p", "parent_pause_seq":0, "run_ref":{"kind":"task", "id":"x"}}),
        json!({"parent_session_id":"p", "parent_pause_seq":1.5, "run_ref":{"kind":"task", "id":"x"}}),
        json!({"parent_session_id":"p", "parent_pause_seq":9007199254740992u64, "run_ref":{"kind":"task", "id":"x"}}),
        json!({"parent_session_id":"p", "parent_pause_seq":1, "run_ref":{"kind":"future", "id":"x"}}),
        json!({"parent_session_id":"p", "parent_pause_seq":1, "run_ref":{"kind":"task", "id":""}}),
    ] {
        value["run_pause"] = pause;
        assert!(parse(&value).is_err());
    }
}
