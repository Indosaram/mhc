use maho_omo_task::subagent_cost_ledger::{create_subagent_cost_ledger, LiveRunStats};
use senpi_task::state::{create_task_record, TaskRecord, TaskRecordInput, TaskRunStats, CostReportStatus};
fn record() -> TaskRecord { create_task_record(TaskRecordInput::default(), Some(1)).expect("record") }
fn cost(usd: f64) -> TaskRunStats { serde_json::from_value(serde_json::json!({"runtime_ms":1,"turns":1,"tool_calls":0,"cost_usd":usd,"cost_status":"reported"})).expect("stats") }
fn live(epoch: i64, usd: f64) -> LiveRunStats { LiveRunStats { run_start_epoch: epoch, stats: cost(usd) } }
#[test] fn live_to_settled_does_not_double_or_drop() {
    let mut ledger = create_subagent_cost_ledger(); let mut record = record();
    ledger.upsert_record(&record); ledger.upsert_live(&record.task_id, Some(&live(0, 0.3)));
    assert!((ledger.total() - 0.3).abs() < 1e-10);
    record.run_stats = Some(cost(0.3)); ledger.upsert_record(&record); ledger.upsert_live(&record.task_id, Some(&live(0, 0.3)));
    assert!((ledger.total() - 0.3).abs() < 1e-10);
}
#[test] fn revived_run_rejects_stale_and_ahead_trackers() {
    let mut ledger = create_subagent_cost_ledger(); let mut record = record(); record.run_stats = Some(cost(0.3));
    ledger.upsert_record(&record); ledger.upsert_live(&record.task_id, Some(&live(1, 0.05)));
    assert!((ledger.total() - 0.3).abs() < 1e-10);
    record.run_stats = None; record.prior_runs_cost_usd = Some(0.3); record.run_start_epoch = 1;
    ledger.upsert_record(&record); ledger.upsert_live(&record.task_id, Some(&live(0, 0.3)));
    assert!((ledger.total() - 0.3).abs() < 1e-10);
    ledger.upsert_live(&record.task_id, Some(&live(1, 0.05)));
    assert!((ledger.total() - 0.35).abs() < 1e-10);
}
#[test] fn unavailable_removed_invalid_and_partial_costs() {
    let mut ledger = create_subagent_cost_ledger(); let mut record = record(); let mut stats = cost(2.0);
    stats.cost_status = Some(CostReportStatus::Unavailable); record.run_stats = Some(stats.clone()); ledger.upsert_record(&record); assert_eq!(ledger.total(), 0.0);
    stats.cost_status = Some(CostReportStatus::Partial); record.run_stats = Some(stats.clone()); ledger.upsert_record(&record); assert_eq!(ledger.total(), 2.0);
    for value in [f64::NAN, f64::INFINITY, -1.0, 0.0] { stats.cost_usd = Some(value); record.run_stats = Some(stats.clone()); ledger.upsert_record(&record); assert_eq!(ledger.total(), 0.0); }
    record.run_stats = Some(cost(2.0)); ledger.upsert_record(&record); ledger.remove(&record.task_id); assert_eq!(ledger.total(), 0.0);
}
#[test] fn legacy_run_key_stays_fixed_across_notification_epoch_changes() {
    let mut ledger = create_subagent_cost_ledger(); let mut record = record(); ledger.upsert_record(&record);
    ledger.upsert_live(&record.task_id, Some(&live(0, 0.2))); record.notification.run_epoch += 1; ledger.upsert_record(&record);
    ledger.upsert_live(&record.task_id, Some(&live(0, 0.25))); assert_eq!(ledger.total(), 0.25);
}
