use std::{collections::BTreeMap, sync::{Arc, Mutex, atomic::{AtomicUsize, Ordering}}};
use maho_ext_api::{ExtensionUi, ExtensionMode};
use maho_omo_task::{runtime_context::TaskRuntimeContext, status_ui::StatusUiManager, subagent_cost_feed::TreeCostFeed, subagent_cost_ledger::LiveRunStats, store_mutation_observer::StoreMutation};
use senpi_task::{state::{TaskRecord, TaskRecordInput, TaskRunStats, CostReportStatus, create_task_record}, manager::child_handle::{ManagedChildListener, ManagedChildEvent, Unsubscribe}};
#[derive(Default)] struct Ui { costs: Mutex<Vec<Option<f64>>> }
impl ExtensionUi for Ui {
    fn supports_subagent_cost(&self) -> bool { true }
    fn set_subagent_cost(&self, cost: Option<f64>) { self.costs.lock().expect("cost feed fixture lock").push(cost); }
    fn select<'a>(&'a self, _: &'a str, _: &'a [String], _: maho_ext_api::ExtensionUiDialogOptions) -> maho_ext_api::UiFuture<'a, Option<String>> { Box::pin(async { None }) }
    fn confirm<'a>(&'a self, _: &'a str, _: &'a str, _: maho_ext_api::ExtensionUiDialogOptions) -> maho_ext_api::UiFuture<'a, bool> { Box::pin(async { false }) }
    fn input<'a>(&'a self, _: &'a str, _: Option<&'a str>, _: maho_ext_api::ExtensionUiDialogOptions) -> maho_ext_api::UiFuture<'a, Option<String>> { Box::pin(async { None }) }
    fn notify(&self, _: &str, _: maho_ext_api::NotificationType) {}
    fn set_status(&self, _: &str, _: Option<&str>) {}
    fn set_widget(&self, _: &str, _: Option<maho_ext_api::WidgetContent>, _: maho_ext_api::ExtensionWidgetOptions) {}
    fn set_header(&self, _: Option<maho_ext_api::ComponentFactory>) {}
    fn set_footer(&self, _: Option<maho_ext_api::ComponentFactory>) {}
    fn set_title(&self, _: &str) {}
    fn paste_to_editor(&self, _: &str) {}
    fn set_editor_text(&self, _: &str) {}
    fn get_editor_text(&self) -> String { String::new() }
    fn custom(&self, _: maho_ext_api::ComponentFactory, _: maho_ext_api::CustomUiOptions) -> maho_ext_api::ExtensionFuture<'_, serde_json::Value> { Box::pin(async { Ok(serde_json::Value::Null) }) }
    fn theme(&self) -> maho_ext_api::Theme { Default::default() }
}
#[derive(Default)] struct Manager { records: Mutex<Vec<TaskRecord>>, reads: AtomicUsize, live: Mutex<BTreeMap<String, LiveRunStats>>, listeners: Arc<Mutex<BTreeMap<String, ManagedChildListener>>> }
impl StatusUiManager for Manager {
    fn list(&self, _: &str) -> Vec<TaskRecord> { panic!("feed must use the entire tree") }
    fn list_all(&self) -> Vec<TaskRecord> { self.reads.fetch_add(1, Ordering::SeqCst); self.records.lock().expect("cost feed fixture lock").clone() }
    fn live_run_stats(&self, task: &str) -> Option<LiveRunStats> { self.live.lock().expect("cost feed fixture lock").get(task).cloned() }
    fn subscribe_child(&self, task: &str, listener: ManagedChildListener) -> Option<Unsubscribe> { self.listeners.lock().expect("cost feed fixture lock").insert(task.into(), listener); let listeners = self.listeners.clone(); let task = task.to_owned(); Some(Box::new(move || { listeners.lock().expect("cost feed fixture lock").remove(&task); })) }
}
fn stats(cost: f64) -> TaskRunStats { TaskRunStats { cost_usd: Some(cost), cost_status: Some(CostReportStatus::Reported), ..Default::default() } }
fn record(parent: &str, own: &str) -> TaskRecord { let mut record = create_task_record(TaskRecordInput { parent_session_id: parent.into(), root_session_id: parent.into(), ..Default::default() }, Some(1)).expect("valid task fixture"); record.child_session_id = Some(own.into()); record }
#[test] fn descendants_seed_once_and_message_events_update_without_frame_scans() {
    let manager = Arc::new(Manager::default()); let parent = record("session", "child"); let grandchild = record("child", "grandchild");
    *manager.records.lock().expect("cost feed fixture lock") = vec![grandchild.clone(), parent.clone()];
    let ui = Arc::new(Ui::default()); let mut context = crate::support::context(); context.ui = ui.clone(); context.mode = ExtensionMode::Tui;
    let mut runtime = TaskRuntimeContext::new("/tmp".into()); runtime.capture_from(&context);
    let feed = TreeCostFeed::new(manager.clone(), Arc::new(Mutex::new(runtime))); feed.ensure_seeded(); feed.ensure_seeded();
    assert_eq!(manager.reads.load(Ordering::SeqCst), 1); assert_eq!(manager.listeners.lock().expect("cost feed fixture lock").len(), 2);
    manager.live.lock().expect("cost feed fixture lock").insert(grandchild.task_id.clone(), LiveRunStats { run_start_epoch: 0, stats: stats(0.2) });
    let listener = manager.listeners.lock().expect("cost feed fixture lock")[&grandchild.task_id].clone();
    listener(&ManagedChildEvent { event_type: "message_end".into(), ..Default::default() });
    assert_eq!(ui.costs.lock().expect("cost feed fixture lock").last(), Some(&Some(0.2))); assert_eq!(manager.reads.load(Ordering::SeqCst), 1);
    let mut settled = grandchild.clone(); settled.execution_mode = "in-process".into(); settled.run_stats = Some(stats(0.2));
    feed.observe(&StoreMutation { previous_status: None, task_id: settled.task_id.clone(), record: Some(settled) });
    assert_eq!(manager.listeners.lock().expect("cost feed fixture lock").len(), 1); assert_eq!(manager.reads.load(Ordering::SeqCst), 1);
    feed.dispose(); assert!(manager.listeners.lock().expect("cost feed fixture lock").is_empty());
}
#[test] fn unsupported_engine_never_scans() {
    let manager = Arc::new(Manager::default()); let context = crate::support::context(); let mut runtime = TaskRuntimeContext::new("/tmp".into()); runtime.capture_from(&context);
    let feed = TreeCostFeed::new(manager.clone(), Arc::new(Mutex::new(runtime))); feed.ensure_seeded(); feed.observe(&StoreMutation { previous_status: None, task_id: "missing".into(), record: None }); assert_eq!(manager.reads.load(Ordering::SeqCst), 0);
}
