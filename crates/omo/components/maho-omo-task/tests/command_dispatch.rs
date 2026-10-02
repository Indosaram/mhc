mod support;
use std::sync::{Arc,Mutex};
use maho_ext_api::*;
use maho_omo_task::commands::{CommandManager,register_task_commands};
use senpi_task::{manager::types::ListScope,state::{TaskRecord,TaskRecordInput,TaskStatus,create_task_record}};
struct Manager { records:Vec<TaskRecord>,cancelled:Mutex<Vec<String>> }
impl CommandManager for Manager {
    fn list(&self,scope:&ListScope)->Vec<TaskRecord> { self.records.iter().filter(|record| match scope { ListScope::All=>true,ListScope::ParentSession(id)=>record.parent_session_id==*id || record.root_session_id==*id }).cloned().collect() }
    fn cancel_task(&self,id:&str,reason:&str)->Result<(),ExtensionFailure> { assert_eq!(reason,"/task-kill"); self.cancelled.lock().expect("cancelled").push(id.into()); Ok(()) }
}
fn manager(status:TaskStatus)->Arc<Manager> { let mut record=create_task_record(TaskRecordInput { parent_session_id:"session".into(),root_session_id:"session".into(),description:Some("Audit the waiting line".into()),..Default::default() },Some(1)).expect("record"); record.status=status; Arc::new(Manager { records:vec![record],cancelled:Mutex::new(vec![]) }) }
async fn invoke(api:&ExtensionApi,name:&str,args:&str,context:&ExtensionContext) { let command=api.registered.commands.iter().find(|command| command.name==name).expect("command"); (command.handler)(args,context).await.expect("command"); }
#[tokio::test] async fn registration_exposes_both_commands() { let mut api=support::api(); register_task_commands(&mut api,manager(TaskStatus::Running)); let mut names=api.registered.commands.iter().map(|command| command.name.as_str()).collect::<Vec<_>>(); names.sort(); assert_eq!(names,["task-kill","tasks"]); }
#[tokio::test] async fn confirmed_multiword_choice_cancels_exact_task_id() { let mut api=support::api(); let manager=manager(TaskStatus::Running); register_task_commands(&mut api,manager.clone()); let ui=Arc::new(support::Ui { select_first:true,confirmed:true,..Default::default() }); let mut context=support::context(); context.ui=ui.clone(); invoke(&api,"task-kill","",&context).await; assert_eq!(*manager.cancelled.lock().expect("cancelled"),[manager.records[0].task_id.clone()]); assert_eq!(ui.selections.lock().expect("selections").len(),1); }
#[tokio::test] async fn dismissed_choice_never_cancels() { let mut api=support::api(); let manager=manager(TaskStatus::Running); register_task_commands(&mut api,manager.clone()); invoke(&api,"task-kill","",&support::context()).await; assert!(manager.cancelled.lock().expect("cancelled").is_empty()); }
#[tokio::test] async fn declined_confirmation_never_cancels() { let mut api=support::api(); let manager=manager(TaskStatus::Running); register_task_commands(&mut api,manager.clone()); let mut context=support::context(); context.ui=Arc::new(support::Ui { select_first:true,..Default::default() }); invoke(&api,"task-kill","",&context).await; assert!(manager.cancelled.lock().expect("cancelled").is_empty()); }
#[tokio::test] async fn terminal_only_set_does_not_open_selector() { let mut api=support::api(); let manager=manager(TaskStatus::Completed); register_task_commands(&mut api,manager.clone()); let ui=Arc::new(support::Ui::default()); let mut context=support::context(); context.ui=ui.clone(); invoke(&api,"task-kill","",&context).await; assert!(ui.selections.lock().expect("selections").is_empty()); assert_eq!(ui.notifications.lock().expect("notifications").len(),1); assert!(manager.cancelled.lock().expect("cancelled").is_empty()); }
#[tokio::test] async fn selector_admits_exactly_the_source_cancellable_statuses() {
    for status in [TaskStatus::Pending,TaskStatus::Running,TaskStatus::Interrupted,TaskStatus::Completed,TaskStatus::Error,TaskStatus::Cancelled,TaskStatus::Lost] {
        let mut api=support::api(); let manager=manager(status); register_task_commands(&mut api,manager.clone()); let ui=Arc::new(support::Ui { select_first:true,confirmed:true,..Default::default() }); let mut context=support::context(); context.ui=ui.clone(); invoke(&api,"task-kill","",&context).await;
        let expected=usize::from(matches!(status,TaskStatus::Pending|TaskStatus::Running|TaskStatus::Interrupted)); assert_eq!(manager.cancelled.lock().expect("cancelled").len(),expected); assert_eq!(ui.selections.lock().expect("selections").len(),expected);
    }
}
