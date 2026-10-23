use crate::support;
use std::sync::Arc;
use maho_ext_api::ExtensionFailure;
use maho_omo_task::dag_commands::{DagCommandManager,register_dag_commands};
use senpi_task::dag::{manager::{DagManager,DagManagerOptions,DagRunSummary,DagStartParams,create_dag_manager},store::{DagStoreConfig,DagStoreOptions,create_dag_file_store},types::DagRunSnapshot};
use senpi_task::dag::types::{DagNodeError,DagNodeErrorCode};
struct Runs(DagManager);
impl DagCommandManager for Runs {
    fn list(&self,session:&str,limit:usize)->Result<Vec<DagRunSummary>,ExtensionFailure> { self.0.list(session,Some(limit)).map_err(|error| ExtensionFailure::new(error.to_string())) }
    fn snapshot(&self,run:&str,session:&str)->Option<DagRunSnapshot> { self.0.snapshot(&run.into(),session).ok() }
    fn task_record(&self,_:&str)->Option<senpi_task::state::TaskRecord> { None }
}
fn fixture()->(tempfile::TempDir,Arc<Runs>) {
    let root=tempfile::tempdir().expect("root"); let store=Arc::new(create_dag_file_store(&DagStoreConfig::new(root.path()),DagStoreOptions::default()).expect("store")); let manager=create_dag_manager(DagManagerOptions { store,new_run_id:None,now:Some(Arc::new(|| 1000)),materialize_skills:None,settings:None }); (root,Arc::new(Runs(manager)))
}
fn start(runs:&Runs,session:&str,name:&str)->String {
    let nodes=[("a",vec![]),("b",vec!["a"]),("c",vec!["a"]),("d",vec!["b","c"])].into_iter().map(|(id,deps)| senpi_task::dag::graph::DagNodeInput { id:id.into(),prompt:"work".into(),target:senpi_task::dag::types::DagNodeTarget::Category("quick".into()),label:None,depends_on:Some(deps.into_iter().map(str::to_owned).collect()),task_summary:None,description:None,load_skills:None }).collect();
    runs.0.start(DagStartParams { definition:senpi_task::dag::graph::DagDefinition { key:name.into(),name:name.into(),nodes },parent_session_id:session.into(),root_session_id:session.into() }).expect("start").snapshot.run_id
}
#[tokio::test] async fn registered_dag_list_is_scoped_to_command_session() {
    let (_root,runs)=fixture(); let owned=start(&runs,"session","owned"); let foreign=start(&runs,"foreign","foreign"); let mut api=support::api(); register_dag_commands(&mut api,runs); let ui=Arc::new(support::Ui::default()); let mut context=support::context(); context.ui=ui.clone(); let command=api.registered.commands.iter().find(|command| command.name=="dag").expect("dag"); (command.handler)("",&context).await.expect("list"); let notifications=ui.notifications.lock().expect("notifications"); assert_eq!(notifications.len(),1); assert!(notifications[0].contains(&owned)); assert!(!notifications[0].contains(&foreign));
}
#[tokio::test] async fn registered_dag_detail_renders_diamond_waves_and_foreign_id_is_hidden() {
    let (_root,runs)=fixture(); let owned=start(&runs,"session","diamond"); let foreign=start(&runs,"foreign","foreign"); let mut api=support::api(); register_dag_commands(&mut api,runs); let ui=Arc::new(support::Ui::default()); let mut context=support::context(); context.ui=ui.clone(); let command=api.registered.commands.iter().find(|command| command.name=="dag").expect("dag"); (command.handler)(&owned,&context).await.expect("detail"); { let notifications=ui.notifications.lock().expect("notifications"); let text=&notifications[0]; assert!(text.contains("wave 1/3")); assert!(text.contains("wave 3/3")); assert!(text.contains("after b, c")); assert!(text.contains("category:quick")); }
    (command.handler)(&foreign,&context).await.expect("foreign"); assert_eq!(ui.notifications.lock().expect("notifications")[1],format!("No dag run \"{foreign}\" in this session."));
}
#[test] fn detail_renders_amendment_count_attempt_badge_error_code_and_rounded_duration() {
    let (_root,runs)=fixture(); let id=start(&runs,"session","formatted");
    let mut snapshot=runs.0.snapshot(&id,"session").expect("snapshot");
    snapshot.amend_history=Some(vec![senpi_task::dag::types::AmendRecord { at:"1970-01-01T00:00:00.000Z".into(),previous_fingerprint:"old".into(),fingerprint:"new".into(),changed_node_ids:vec![],added_node_ids:vec![],invalidated_node_ids:vec![] }]);
    let node=&mut snapshot.nodes[0]; node.attempt=2; node.started_at=Some("1970-01-01T00:00:00.000Z".into()); node.completed_at=Some("1970-01-01T00:00:01.249Z".into()); node.error=Some(DagNodeError { code:DagNodeErrorCode::TaskError,message:"failed".into(),node_id:None,at:"1970-01-01T00:00:01.249Z".into() });
    let text=maho_omo_task::dag_commands::detail_text(&snapshot,runs.as_ref()).join("\n");
    assert!(text.contains(", amended x1")); assert!(text.contains(" x2 ")); assert!(text.contains("error: task_error failed"));
    assert!(text.contains("1.2s"));
}
