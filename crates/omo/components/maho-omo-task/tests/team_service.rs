use std::{collections::BTreeSet,sync::{Arc,Mutex}};
use maho_omo_task::team_service::{TeamService,TeamServiceDeps,create_team_service};
use senpi_task::{manager::{create_task_manager,types::{ManagedRunner,ManagedRunnerResult,ManagedStartSpec,ManagedRunners,TaskManagerOptions,ResolvedChildPlan}},store::{StateDirConfig,TaskRecordStore},team::{runtime_types::*,runtime_config::{TeamTaskBounds,to_team_core_config},storage::team_storage_base_dir,normalize::normalize_senpi_team_spec,member_projection::ResidentSessionRef},lifecycle::DestroyCause,tools::team::types::*};
use team_core::{team_state_store::{create_runtime_state,transition_runtime_state},types::{SpecSource,RuntimeStatus,TaskStatus}};
use serde_json::json;
struct NoLaunch;
impl ManagedRunner for NoLaunch { fn start(&self,_:&ManagedStartSpec)->ManagedRunnerResult { panic!("unexpected child launch") } }
struct Members;
impl TeamMemberReadPort for Members { fn get(&self,_:&str)->Option<TeamMemberTaskRecord> { None } }
impl TeamMemberCancelPort for Members { fn cancel_task(&self,_:&str,_:Option<&str>)->TeamCancelOutcome { panic!("unexpected cancellation") } }
impl TeamRuntimeManagerPort for Members { fn start(&self,_:&TeamMemberStartSpec)->Result<TeamStartResult,String> { panic!("unexpected member launch") } fn get_resident_handle(&self,_:&str)->Option<ResidentSessionRef> { None } }
impl TeamMemberDestructionPort for Members { fn destroy_resident_task(&self,_:&str,_:DestroyCause)->Result<(),String> { panic!("unexpected destruction") } }
struct Fixture { service:TeamService,run:String,session:Arc<Mutex<Option<String>>>,_root:tempfile::TempDir }
fn fixture()->Fixture {
    let root=tempfile::tempdir().expect("team root"); let state_dir=StateDirConfig { project_dir:root.path().into(),task_state_dir:None }; let bounds=TeamTaskBounds { max_members:4,max_parallel_members:2,max_wall_clock_minutes:10 };
    let manager=create_task_manager(TaskManagerOptions::new(TaskRecordStore::new(&state_dir),ManagedRunners { in_process:Arc::new(NoLaunch),process:Arc::new(NoLaunch) },Arc::new(|_| Ok(ResolvedChildPlan { model:"faux/faux".into(),..Default::default() })),root.path().to_string_lossy()));
    let config=to_team_core_config(&bounds,&team_storage_base_dir(&state_dir).to_string_lossy()).expect("config"); let spec=normalize_senpi_team_spec(&json!({"members":[{"name":"beta","kind":"category","category":"quick","prompt":"work"}]}),"squad",None).expect("spec");
    let state=create_runtime_state(&spec,Some("lead"),SpecSource::Project,&config).expect("runtime"); let run=state.team_run_id;
    transition_runtime_state(&run,|mut state| { state.status=RuntimeStatus::Active; state },&config).expect("active");
    let session=Arc::new(Mutex::new(Some("lead".into()))); let id=session.clone(); let service=create_team_service(TeamServiceDeps { manager:Arc::new(manager),member_manager:Arc::new(Members),destruction:Arc::new(Members),session_id:Arc::new(move || id.lock().expect("session").clone()),state_dir,bounds,omo_config:json!({}),agent_names:BTreeSet::new(),member_extension:TeamMemberExtensionConfig::default(),append_task_event:Some(Arc::new(|_,_| {})),now:Some(Arc::new(|| 1000)),new_message_id:None }).expect("service");
    Fixture { service,run,session,_root:root }
}
fn input()->CreateTeamTaskServiceInput { CreateTeamTaskServiceInput { subject:"work".into(),description:"do work".into(),status:TaskStatus::Pending,owner:None,blocked_by:None } }
#[test] fn task_create_list_get_share_real_persisted_state() { let f=fixture(); let task=f.service.create_task(&f.run,&input()).expect("task"); assert_eq!(f.service.get_task(&f.run,&task.id).expect("get"),task); assert_eq!(f.service.list_tasks(&f.run,None).expect("list"),vec![task]); }
#[test] fn foreign_session_cannot_read_or_mutate_tasklist() { let f=fixture(); *f.session.lock().expect("session")=Some("foreign".into()); assert!(f.service.create_task(&f.run,&input()).is_err()); assert!(f.service.list_tasks(&f.run,None).is_err()); assert!(f.service.status(&f.run).is_err()); }
#[test] fn uncaptured_session_can_read_existing_team() { let f=fixture(); *f.session.lock().expect("session")=None; assert!(f.service.list_tasks(&f.run,None).expect("list").is_empty()); }
#[test] fn team_listing_preserves_owner_and_scope() { let f=fixture(); let teams=f.service.list_teams().expect("teams"); assert_eq!(teams.len(),1); assert_eq!(teams[0].team_run_id,f.run); assert_eq!(teams[0].lead_session_id.as_deref(),Some("lead")); }
#[test] fn malformed_delete_id_rejected_before_runtime_access() { let f=fixture(); assert!(f.service.delete_team(&DeleteTeamToolInput { team_run_id:"../foreign".into(),force:None }).is_err()); }
#[test] fn absent_lead_session_rejects_team_creation_before_launch() { let f=fixture(); *f.session.lock().expect("session")=None; assert!(f.service.create_team(&CreateTeamToolInput { team_name:None,inline_spec:Some(json!({"members":[]})) }).is_err()); }
#[test] fn all_scoped_methods_reject_foreign_session_before_side_effects() {
    let f=fixture(); *f.session.lock().expect("session")=Some("foreign".into());
    assert!(f.service.get_task(&f.run,"1").is_err());
    assert!(f.service.update_task(&UpdateTeamTaskServiceInput { team_run_id:f.run.clone(),task_id:"1".into(),status:TaskStatus::Completed,owner:None }).is_err());
    assert!(f.service.send_message(&f.run,&senpi_task::team::messaging::types::SendTeamMessageInput { from:"lead".into(),to:"beta".into(),body:"work".into(),summary:None }).is_err());
    assert!(f.service.delete_team(&DeleteTeamToolInput { team_run_id:f.run.clone(),force:None }).is_err());
    assert!(f.service.request_shutdown(&f.run,"beta").is_err());
    assert!(f.service.approve_shutdown(&f.run,"beta").is_err());
    assert!(f.service.reject_shutdown(&f.run,"beta","continue").is_err());
}
#[test] fn task_claim_defaults_to_lead_and_filter_reads_persisted_owner() {
    let f=fixture(); let task=f.service.create_task(&f.run,&input()).expect("create");
    let claimed=f.service.update_task(&UpdateTeamTaskServiceInput { team_run_id:f.run.clone(),task_id:task.id,status:TaskStatus::Claimed,owner:None }).expect("claim");
    assert_eq!(claimed.owner.as_deref(),Some("lead"));
    assert_eq!(f.service.list_tasks(&f.run,Some(&TeamTaskListFilter { status:Some(TaskStatus::Claimed),owner:Some("lead".into()) })).expect("filter"),vec![claimed]);
    assert!(f.service.list_tasks(&f.run,Some(&TeamTaskListFilter { status:Some(TaskStatus::Pending),owner:None })).expect("pending").is_empty());
}
