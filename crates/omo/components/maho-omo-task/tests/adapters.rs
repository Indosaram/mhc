use std::{collections::BTreeMap, sync::Arc};
use maho_ext_api::{ExtensionFailure, NotificationType};
use maho_omo_task::{commands::{CommandManager, task_list_text, kill_option}, dag_commands::{DagCommandManager, dag_command_text, detail_text}, dag_status_row_format::run_rows, dag_tool::{DagToolDeps, run_dag_tool}, planner::create_task_child_planner, reload_guard::evaluate_reload_veto, renderers::{render_category_unavailable, render_task_completion, render_team_member_liveness, TeamMemberLivenessDetails}, status_row_format::{background_widget_rows, build_widget_rows, format_task_row}, surface::missing_task_capabilities, task_rpc_codec::{parse_task_cancel, parse_task_output, parse_task_send}};
use serde_json::json;
use senpi_task::{dag::{manager::{DagManager, DagManagerOptions, DagRunSummary, create_dag_manager}, store::{DagStoreConfig, DagStoreOptions, create_dag_file_store}, types::DagRunSnapshot}, manager::types::{ListScope, ManagerStartSpec, PlanResolutionCode}, state::{TaskRecord, TaskRecordInput, TaskStatus, ResidencyState, create_task_record}};

fn record(id: &str, status: TaskStatus) -> TaskRecord {
    let mut record = create_task_record(TaskRecordInput { parent_session_id: "session-a".into(), root_session_id: "session-a".into(), execution_mode: "in-process".into(), model: "faux/faux-1".into(), ..TaskRecordInput::default() }, Some(1)).expect("valid record fixture");
    record.task_id = id.into(); record.status = status; record.created_at = "2026-07-07T00:00:00.000Z".into(); record
}
struct Tasks(Vec<TaskRecord>);
impl CommandManager for Tasks {
    fn list(&self, scope: &ListScope) -> Vec<TaskRecord> { self.0.iter().filter(|record| match scope { ListScope::All => true, ListScope::ParentSession(id) => &record.parent_session_id == id || &record.root_session_id == id }).cloned().collect() }
    fn cancel_task(&self, _: &str, _: &str) -> Result<(), ExtensionFailure> { Ok(()) }
}
fn dag() -> (tempfile::TempDir, DagToolDeps) {
    let root = tempfile::tempdir().expect("temporary project");
    let store = create_dag_file_store(&DagStoreConfig::new(root.path()), DagStoreOptions::default()).expect("DAG store");
    let manager = create_dag_manager(DagManagerOptions { store: Arc::new(store), new_run_id: Some(Arc::new(|| "run-1".into())), now: Some(Arc::new(|| 1)), materialize_skills: None, settings: None });
    (root, DagToolDeps { manager, parent_session_id: Arc::new(|| "session-a".into()), root_session_id: Arc::new(|| "session-a".into()), wait: None, cancel: None })
}
fn start(deps: &DagToolDeps) -> String {
    let result = run_dag_tool(deps, serde_json::from_value(json!({"action":"start","definition":{"key":"plan","name":"release plan","nodes":[{"id":"a","prompt":"draft plan","category":"quick"},{"id":"b","prompt":"build it","subagent_type":"explore","model":"faux/faux-1","dependsOn":["a"]}]}})).expect("start input")).expect("start result");
    result.details.expect("start details")["run_id"].as_str().expect("run id").into()
}
struct Runs(DagManager);
impl DagCommandManager for Runs {
    fn list(&self, session: &str, limit: usize) -> Result<Vec<DagRunSummary>, ExtensionFailure> { self.0.list(session, Some(limit)).map_err(|error| ExtensionFailure::new(error.to_string())) }
    fn snapshot(&self, run: &str, session: &str) -> Option<DagRunSnapshot> { self.0.snapshot(&run.into(), session).ok() }
    fn task_record(&self, _: &str) -> Option<TaskRecord> { None }
}

#[test] fn task_list_scopes_current_session() { let mine = record("st_mine", TaskStatus::Running); let mut other = record("st_other", TaskStatus::Running); other.parent_session_id = "other".into(); other.root_session_id = "other".into(); let text = task_list_text(&Tasks(vec![mine, other]), "", Some("session-a")); assert!(text.contains("st_mine")); assert!(!text.contains("st_other")); }
#[test] fn task_list_all_includes_foreign_session() { let mut other = record("st_other", TaskStatus::Running); other.parent_session_id = "other".into(); let text = task_list_text(&Tasks(vec![other]), "--all", None); assert!(text.contains("st_other")); }
#[test] fn task_list_missing_session_fails_closed() { assert_eq!(task_list_text(&Tasks(vec![record("st_mine", TaskStatus::Running)]), "", None), "No tasks in this session."); }
#[test] fn kill_option_preserves_multiword_identity() { let mut record = record("st_kill", TaskStatus::Running); record.description = Some("Audit the waiting line".into()); assert_eq!(kill_option(&record), "Audit the waiting line (st_kill) running"); }
#[test] fn task_summary_wins_over_description() { let mut record = record("st_1", TaskStatus::Running); record.task_summary = Some("summary".into()); record.description = Some("description".into()); assert!(format_task_row(&record).starts_with("summary (st_1)")); }
#[test] fn suspended_row_overrides_running_label() { let mut record = record("st_1", TaskStatus::Running); record.residency_state = ResidencyState::RpcDetached; assert!(format_task_row(&record).contains("status:suspended")); }
#[test] fn terminal_tasks_leave_widget() { assert!(build_widget_rows(&[record("st_1", TaskStatus::Completed)]).is_empty()); }
#[test] fn widget_overflow_reports_remaining_tasks() { let records: Vec<_> = (0..7).map(|id| record(&format!("st_{id}"), TaskStatus::Running)).collect(); let rows = build_widget_rows(&records); assert_eq!(rows.len(), 6); assert_eq!(rows[5], "+2 more"); }
#[test] fn background_rows_respect_actual_width() { let rows = background_widget_rows(&[record("st_1", TaskStatus::Running)], &BTreeMap::new(), 1783382401000, &|_| None, Some(40)); assert!(rows.iter().all(|row| senpi_task::renderer_text::renderer_visible_width(row) <= 40)); }
#[test] fn background_spinner_advances_with_clock() { let records = [record("st_1", TaskStatus::Running)]; let first = background_widget_rows(&records, &BTreeMap::new(), 0, &|_| None, Some(220)); let second = background_widget_rows(&records, &BTreeMap::new(), 250, &|_| None, Some(220)); assert_ne!(first, second); }
#[test] fn empty_completion_uses_fallback() { assert_eq!(render_task_completion(&[], 80), ["(task completion)"]); }
#[test] fn liveness_sanitizes_control_injection() { let rows = render_team_member_liveness(Some(&TeamMemberLivenessDetails { member_name: "alpha", last_known_state: "error", reason: Some("\x1b[31mfailed\x1b[0m\nnow") })); assert_eq!(rows[3], "reason:failed now"); }
#[test] fn absent_liveness_details_use_fallback() { assert_eq!(render_team_member_liveness(None), ["(team member liveness)"]); }
#[test] fn category_warning_sanitizes_content() { assert_eq!(render_category_unavailable(Some("\x1b[31munavailable\x1b[0m")), ["unavailable"]); }
#[test] fn category_warning_empty_content_uses_fallback() { assert_eq!(render_category_unavailable(Some("")), ["(category unavailable)"]); }
#[test] fn reload_running_child_vetoes() { assert!(evaluate_reload_veto(&[record("st_1", TaskStatus::Running)]).unwrap().cancelled); }
#[test] fn reload_terminal_child_does_not_veto() { assert!(evaluate_reload_veto(&[record("st_1", TaskStatus::Completed)]).is_none()); }
#[test] fn reload_empty_set_does_not_veto() { assert!(evaluate_reload_veto(&[]).is_none()); }
#[test] fn capabilities_report_missing_in_source_order() { assert_eq!(missing_task_capabilities(false, false), ["sendMessage", "registerMessageRenderer"]); }
#[test] fn capabilities_complete_surface_passes() { assert!(missing_task_capabilities(true, true).is_empty()); }
#[test] fn send_codec_rejects_foreign_identifier() { assert!(parse_task_send(&json!({"to":"team/member","message":"hello"})).is_err()); }
#[test] fn send_codec_keeps_plain_text() { let parsed = parse_task_send(&json!({"to":"st_1","message":"hello"})).unwrap(); assert_eq!(parsed.to, "st_1"); }
#[test] fn send_codec_bounds_utf16_units() { assert!(parse_task_send(&json!({"to":"st_1","message":"😀".repeat(16001)})).is_err()); }
#[test] fn cancel_codec_rejects_nonstring_reason() { assert!(parse_task_cancel(&json!({"task_id":"st_1","reason":true})).is_err()); }
#[test] fn cancel_codec_keeps_reason() { assert_eq!(parse_task_cancel(&json!({"task_id":"st_1","reason":"stop"})).unwrap().reason.as_deref(), Some("stop")); }
#[test] fn output_codec_rejects_oversize_tail() { assert!(parse_task_output(&json!({"task_id":"st_1","tail_lines":1001})).is_err()); }
#[test] fn output_codec_rejects_invalid_mode() { assert!(parse_task_output(&json!({"task_id":"st_1","mode":"wait"})).is_err()); }
#[test] fn output_codec_accepts_tail_boundary() { assert_eq!(parse_task_output(&json!({"task_id":"st_1","mode":"tail","tail_lines":1000})).unwrap().tail_lines, Some(1000)); }
#[test] fn planner_explicit_model_never_reads_registry() { let planner = create_task_child_planner(json!({}), vec![], Arc::new(|| panic!("registry should not be accessed"))); let result = planner(&ManagerStartSpec { model: Some("faux/faux-1".into()), ..ManagerStartSpec::default() }).unwrap(); assert_eq!(result.model, "faux/faux-1"); }
#[test] fn planner_missing_registry_fails_closed() { let planner = create_task_child_planner(json!({}), vec![], Arc::new(|| None)); let error = planner(&ManagerStartSpec { category: Some("quick".into()), ..ManagerStartSpec::default() }).unwrap_err(); assert_eq!(error.code, PlanResolutionCode::ModelUnavailable); }
#[test] fn dag_start_reuses_identical_definition() { let (_root, deps) = dag(); let first = start(&deps); let second = start(&deps); assert_eq!(first, second); assert_eq!(deps.manager.list("session-a", None).unwrap().len(), 1); }
#[test] fn dag_snapshot_roundtrips_owned_run() { let (_root, deps) = dag(); let id = start(&deps); let result = run_dag_tool(&deps, serde_json::from_value(json!({"action":"snapshot","run_id":id})).unwrap()).unwrap(); assert_eq!(result.details.unwrap()["kind"], "snapshot"); }
#[test] fn dag_requires_definition_for_start() { let (_root, deps) = dag(); let result = run_dag_tool(&deps, serde_json::from_value(json!({"action":"start"})).unwrap()).unwrap(); assert_eq!(result.details.unwrap()["error"]["code"], "invalid_definition"); }
#[test] fn dag_rejects_category_with_model() { let (_root, deps) = dag(); let result = run_dag_tool(&deps, serde_json::from_value(json!({"action":"start","definition":{"key":"x","name":"x","nodes":[{"id":"a","prompt":"x","category":"quick","model":"faux/faux-1"}]}})).unwrap()).unwrap(); assert_eq!(result.details.unwrap()["error"]["nodes"][0]["code"], "category_with_model"); }
#[test] fn dag_foreign_session_cannot_read_run() { let (_root, mut deps) = dag(); let id = start(&deps); deps.parent_session_id = Arc::new(|| "other".into()); let result = run_dag_tool(&deps, serde_json::from_value(json!({"action":"snapshot","run_id":id})).unwrap()).unwrap(); assert_eq!(result.details.unwrap()["error"]["code"], "run_not_owned"); }
#[test] fn dag_wait_without_scheduler_returns_current_snapshot() { let (_root, deps) = dag(); let id = start(&deps); let result = run_dag_tool(&deps, serde_json::from_value(json!({"action":"wait","run_id":id})).unwrap()).unwrap(); assert_eq!(result.details.unwrap()["result"]["status"], "pending"); }
#[test] fn dag_command_unknown_run_warns() { let (_root, deps) = dag(); let (_, kind) = dag_command_text(&Runs(deps.manager), "unknown", Some("session-a")).unwrap(); assert_eq!(kind, NotificationType::Warning); }
#[test] fn dag_command_without_session_is_empty() { let (_root, deps) = dag(); assert_eq!(dag_command_text(&Runs(deps.manager), "", None).unwrap().0, "No dag runs in this session."); }
#[test] fn dag_details_preserve_dependencies_and_critical_path() { let (_root, deps) = dag(); let id = start(&deps); let snapshot = deps.manager.snapshot(&id, "session-a").unwrap(); let text = detail_text(&snapshot, &Runs(deps.manager)).join("\n"); assert!(text.contains("after a")); assert!(text.contains("critical path: a -> b")); }
#[test] fn dag_status_tracks_first_unsettled_wave() { let (_root, deps) = dag(); let id = start(&deps); let snapshot = deps.manager.snapshot(&id, "session-a").unwrap(); assert!(run_rows(&snapshot, None)[0].contains("wave 1/2")); }
