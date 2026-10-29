use std::{path::PathBuf,sync::{Arc,Mutex,atomic::{AtomicUsize,Ordering}}};
use maho_omo_task::{lead_poller_lifecycle::{create_lead_poller_lifecycle,LeadPollerLifecycle,LeadPollerLifecycleDeps,DefaultTeamRunIdResolution},status_ui::StatusUiTimers};
use senpi_task::{completion::ParentState,team::{runtime_config::{to_team_core_config,TeamTaskBounds},messaging::lead_poller_types::{LeadPoller,LeadPollFilter,LeadPollError,LeadInjectionSink,LeadInjection}},tools::team::types::{ActiveTeamSummary,ActiveTeamScope}};
#[derive(Default)] struct Timers;
impl StatusUiTimers for Timers { fn set(&self,_:Box<dyn FnOnce()+Send>,_:u64)->u64 { 1 } fn clear(&self,_:u64) {} }
#[derive(Default)] struct Poller { polls:AtomicUsize,stops:AtomicUsize }
impl LeadPoller for Poller { fn poll_once(&self,_:Option<&LeadPollFilter>)->Result<(),LeadPollError> { self.polls.fetch_add(1,Ordering::SeqCst); Ok(()) } fn shutdown(&self) { self.stops.fetch_add(1,Ordering::SeqCst); } }
struct Sink; impl LeadInjectionSink for Sink { fn enqueue(&self,_:LeadInjection) {} }
struct Fixture { lifecycle:Arc<LeadPollerLifecycle>, teams:Arc<Mutex<Vec<ActiveTeamSummary>>>, session:Arc<Mutex<Option<String>>>, file:Arc<Mutex<Option<PathBuf>>>, state:Arc<Mutex<ParentState>>, poller:Arc<Poller>, _root:tempfile::TempDir }
fn team(id:&str,owner:&str)->ActiveTeamSummary { ActiveTeamSummary { team_run_id:id.into(),team_name:id.into(),status:"active".into(),member_count:1,scope:ActiveTeamScope::Project,lead_session_id:Some(owner.into()) } }
fn fixture()->Fixture {
    fixture_with_journal(None)
}
fn fixture_with_journal(journal:Option<Arc<senpi_task::team::messaging::delivery_journal::LeadDeliveryJournal>>)->Fixture {
    let root = tempfile::tempdir().expect("temp root"); let teams=Arc::new(Mutex::new(vec![team("a","lead")])); let session=Arc::new(Mutex::new(Some("lead".into()))); let file=Arc::new(Mutex::new(Some(root.path().join("session.jsonl")))); let state=Arc::new(Mutex::new(ParentState::Idle)); let poller=Arc::new(Poller::default());
    let config=to_team_core_config(&TeamTaskBounds { max_members:4,max_parallel_members:2,max_wall_clock_minutes:10 },root.path().to_str().expect("root path")).expect("config");
    let list=teams.clone(); let id=session.clone(); let path=file.clone(); let parent=state.clone(); let created=poller.clone(); let runtime=root.path().to_path_buf();
    let expected_journal=journal.clone();
    let lifecycle=create_lead_poller_lifecycle(LeadPollerLifecycleDeps { list_teams:Arc::new(move || Ok(list.lock().expect("teams").clone())),session_id:Arc::new(move || id.lock().expect("session").clone()),session_file:Arc::new(move || path.lock().expect("file").clone()),parent_state:Arc::new(move || *parent.lock().expect("state")),config,runtime_dir:Arc::new(move |run| runtime.join(run)),delivery_journal:journal,append_event:Arc::new(|_,_| {}),sink:Arc::new(Sink),factory:Some(Arc::new(move |deps| { match (&expected_journal,&deps.delivery_journal) { (Some(expected),Some(actual))=>assert!(Arc::ptr_eq(expected,actual)),(None,None)=>{},_=>panic!("journal identity lost") } created.clone() })),timers:Arc::new(Timers),on_error:Arc::new(|error| panic!("{error}")) });
    Fixture { lifecycle,teams,session,file,state,poller,_root:root }
}
#[test] fn tick_polls_only_owned_teams() { let f=fixture(); f.teams.lock().expect("teams").push(team("foreign","other")); f.lifecycle.tick().expect("tick"); assert_eq!(f.poller.polls.load(Ordering::SeqCst),1); assert!(f.lifecycle.resolve_lead_poller("foreign").is_none()); }
#[test] fn shared_delivery_journal_reaches_factory_and_owned_poller_is_reused() {
    let journal=Arc::new(senpi_task::team::messaging::delivery_journal::create_lead_delivery_journal(Default::default())); let f=fixture_with_journal(Some(journal)); f.lifecycle.tick().expect("initial"); let first=f.lifecycle.resolve_lead_poller("a").expect("poller"); f.lifecycle.tick().expect("again"); let second=f.lifecycle.resolve_lead_poller("a").expect("poller"); assert!(Arc::ptr_eq(&first,&second)); assert_eq!(f.poller.polls.load(Ordering::SeqCst),2); f.lifecycle.shutdown(); assert_eq!(f.poller.stops.load(Ordering::SeqCst),1);
}
#[test] fn switching_sessions_stops_stale_poller() { let f=fixture(); f.lifecycle.tick().expect("tick"); *f.session.lock().expect("session")=Some("other".into()); f.lifecycle.tick().expect("tick"); assert_eq!(f.poller.stops.load(Ordering::SeqCst),1); assert!(f.lifecycle.resolve_lead_poller("a").is_none()); }
#[test] fn no_session_file_prevents_polling() { let f=fixture(); *f.file.lock().expect("file")=None; f.lifecycle.tick().expect("tick"); assert_eq!(f.poller.polls.load(Ordering::SeqCst),0); }
#[test] fn transitions_suppress_polling_and_resolution() { for state in [ParentState::Compacting,ParentState::SessionSwitching,ParentState::SessionShutdown] { let f=fixture(); *f.state.lock().expect("state")=state; f.lifecycle.tick().expect("tick"); assert_eq!(f.poller.polls.load(Ordering::SeqCst),0); assert!(f.lifecycle.resolve_lead_poller("a").is_none()); } }
#[test] fn streaming_parent_can_poll() { let f=fixture(); *f.state.lock().expect("state")=ParentState::Streaming; f.lifecycle.tick().expect("tick"); assert_eq!(f.poller.polls.load(Ordering::SeqCst),1); }
#[test] fn one_owned_team_resolves_default() { let f=fixture(); assert_eq!(f.lifecycle.resolve_team_run_id(None).expect("team"),"a"); assert!(matches!(f.lifecycle.resolve_default_team_run_id().expect("resolution"),DefaultTeamRunIdResolution::Resolved(id) if id=="a")); }
#[test] fn no_owned_team_has_no_default() { let f=fixture(); f.teams.lock().expect("teams").clear(); assert!(f.lifecycle.resolve_team_run_id(None).is_err()); assert!(matches!(f.lifecycle.resolve_default_team_run_id().expect("resolution"),DefaultTeamRunIdResolution::None)); }
#[test] fn multiple_owned_teams_require_explicit_selection() { let f=fixture(); f.teams.lock().expect("teams").push(team("b","lead")); assert!(f.lifecycle.resolve_team_run_id(None).is_err()); assert_eq!(f.lifecycle.resolve_team_run_id(Some("b")).expect("team"),"b"); assert!(matches!(f.lifecycle.resolve_default_team_run_id().expect("resolution"),DefaultTeamRunIdResolution::Ambiguous(_))); }
#[test] fn explicit_foreign_team_is_rejected() { let f=fixture(); assert!(f.lifecycle.resolve_team_run_id(Some("foreign")).is_err()); }
#[test] fn suspended_lead_retains_poller_and_resumes_without_shutdown() {
    let f=fixture(); f.lifecycle.tick().expect("initial"); let initial=f.lifecycle.resolve_lead_poller("a").expect("poller");
    *f.state.lock().expect("state")=ParentState::SessionShutdown; let file=f.file.lock().expect("file").take(); f.lifecycle.tick().expect("suspended"); assert!(f.lifecycle.resolve_lead_poller("a").is_none());
    *f.state.lock().expect("state")=ParentState::Idle; *f.file.lock().expect("file")=file; f.lifecycle.tick().expect("resume"); let resumed=f.lifecycle.resolve_lead_poller("a").expect("poller"); assert!(Arc::ptr_eq(&initial,&resumed)); assert_eq!(f.poller.stops.load(Ordering::SeqCst),0); assert_eq!(f.poller.polls.load(Ordering::SeqCst),2); f.lifecycle.shutdown();
}
#[test] fn disappeared_ownership_shuts_down_even_when_team_id_stays() {
    let f=fixture(); f.lifecycle.tick().expect("initial"); *f.teams.lock().expect("teams")=vec![team("a","foreign")]; f.lifecycle.tick().expect("reconcile"); assert!(f.lifecycle.resolve_lead_poller("a").is_none()); assert_eq!(f.poller.stops.load(Ordering::SeqCst),1); assert_eq!(f.poller.polls.load(Ordering::SeqCst),1); f.lifecycle.shutdown();
}
#[test] fn shutdown_is_idempotent_and_prevents_future_ticks() { let f=fixture(); f.lifecycle.tick().expect("tick"); f.lifecycle.shutdown(); f.lifecycle.shutdown(); f.lifecycle.tick().expect("tick"); assert_eq!(f.poller.stops.load(Ordering::SeqCst),1); assert_eq!(f.poller.polls.load(Ordering::SeqCst),1); }
