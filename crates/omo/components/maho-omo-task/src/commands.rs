use std::sync::Arc;
use maho_ext_api::{ExtensionApi, ExtensionContext, ExtensionFailure, ExtensionUiDialogOptions, NotificationType};
use senpi_task::{manager::{TaskManager, types::ListScope}, state::{TaskRecord, TaskStatus}, steering::CancelOptions};
use crate::status_row_format::{format_task_row, task_identity};

pub trait CommandManager: Send + Sync {
    fn list(&self, scope: &ListScope) -> Vec<TaskRecord>;
    fn cancel_task(&self, id: &str, reason: &str) -> Result<(), ExtensionFailure>;
}
impl CommandManager for TaskManager {
    fn list(&self, scope: &ListScope) -> Vec<TaskRecord> { self.list(scope).into_iter().map(|entry| entry.record).collect() }
    fn cancel_task(&self, id: &str, reason: &str) -> Result<(), ExtensionFailure> {
        self.cancel_task(id, Some(reason), CancelOptions::default()).map(|_| ()).map_err(|error| ExtensionFailure::new(error.to_string()))
    }
}
pub fn task_list_text(manager: &dyn CommandManager, args: &str, session: Option<&str>) -> String {
    let all = args.split_whitespace().any(|token| token == "--all");
    let records = if all { manager.list(&ListScope::All) } else { session.map_or_else(Vec::new, |id| manager.list(&ListScope::ParentSession(id.into()))) };
    if records.is_empty() { format!("No tasks in {}.", if all { "all sessions" } else { "this session" }) } else { records.iter().map(format_task_row).collect::<Vec<_>>().join("\n") }
}
pub fn kill_option(record: &TaskRecord) -> String {
    let identity = task_identity(record);
    let suffix = if identity == record.task_id { String::new() } else { format!(" ({})", record.task_id) };
    format!("{identity}{suffix} {}", record.status.as_str())
}
pub fn register_task_commands(api: &mut ExtensionApi, manager: Arc<dyn CommandManager>) {
    let list_manager = manager.clone();
    api.register_command("tasks", Some("List session tasks (--all for every session).".into()), None, Arc::new(move |args, ctx| {
        let manager = list_manager.clone();
        Box::pin(async move { let session = ctx.session_manager.session_id(); ctx.ui.notify(&task_list_text(manager.as_ref(), args, Some(session)), NotificationType::Info); Ok(()) })
    }));
    api.register_command("task-kill", Some("Cancel a session task via selector.".into()), None, Arc::new(move |_, ctx| {
        let manager = manager.clone();
        Box::pin(async move { run_task_kill(manager.as_ref(), ctx).await })
    }));
}
pub async fn run_task_kill(manager: &dyn CommandManager, ctx: &ExtensionContext) -> Result<(), ExtensionFailure> {
    let records: Vec<_> = manager.list(&ListScope::ParentSession(ctx.session_manager.session_id().into())).into_iter().filter(|record| matches!(record.status, TaskStatus::Running | TaskStatus::Pending | TaskStatus::Interrupted)).collect();
    if records.is_empty() { ctx.ui.notify("No cancellable tasks.", NotificationType::Info); return Ok(()) }
    let options: Vec<_> = records.iter().map(kill_option).collect();
    let Some(choice) = ctx.ui.select("Cancel which task?", &options, ExtensionUiDialogOptions::default()).await else { return Ok(()) };
    let Some(index) = options.iter().position(|option| option == &choice) else { return Ok(()) };
    let selected = &records[index];
    if !ctx.ui.confirm("Cancel task", &format!("Cancel {}?", selected.task_id), ExtensionUiDialogOptions::default()).await { return Ok(()) }
    manager.cancel_task(&selected.task_id, "/task-kill")?;
    ctx.ui.notify(&format!("Cancelled {}.", selected.task_id), NotificationType::Info);
    Ok(())
}
