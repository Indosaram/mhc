use std::{collections::BTreeMap, path::PathBuf, sync::{Arc, Mutex, PoisonError, atomic::{AtomicBool, Ordering}}};
use senpi_task::{completion::ParentState, team::{member_map::read_member_task_map, messaging::{delivery_journal::LeadDeliveryJournal, lead_poller::create_lead_poller, lead_poller_types::{LeadInjectionSink, LeadPoller, LeadPollerDeps, TeamTaskEvent}}, runtime_config::TeamCoreConfig}, tools::team::types::ActiveTeamSummary};
use crate::status_ui::StatusUiTimers;
pub type TeamListing = Arc<dyn Fn() -> Result<Vec<ActiveTeamSummary>, String> + Send + Sync>;
pub type LeadPollerFactory = Arc<dyn Fn(LeadPollerDeps) -> Arc<dyn LeadPoller + Send + Sync> + Send + Sync>;
pub type AppendTeamEvent = Arc<dyn Fn(&str, TeamTaskEvent) + Send + Sync>;
pub struct LeadPollerLifecycleDeps {
    pub list_teams: TeamListing, pub session_id: Arc<dyn Fn() -> Option<String> + Send + Sync>, pub session_file: Arc<dyn Fn() -> Option<PathBuf> + Send + Sync>, pub parent_state: Arc<dyn Fn() -> ParentState + Send + Sync>, pub config: TeamCoreConfig,
    pub runtime_dir: Arc<dyn Fn(&str) -> PathBuf + Send + Sync>, pub delivery_journal: Option<Arc<LeadDeliveryJournal>>, pub append_event: AppendTeamEvent, pub sink: Arc<dyn LeadInjectionSink>, pub factory: Option<LeadPollerFactory>, pub timers: Arc<dyn StatusUiTimers>, pub on_error: Arc<dyn Fn(String) + Send + Sync>,
}
struct OwnedPoller { owner: String, poller: Arc<dyn LeadPoller + Send + Sync> }
pub struct LeadPollerLifecycle { deps: LeadPollerLifecycleDeps, pollers: Mutex<BTreeMap<String, OwnedPoller>>, operation: Mutex<()>, timer: Mutex<Option<u64>>, stopped: AtomicBool }
pub enum DefaultTeamRunIdResolution { Resolved(String), None, Ambiguous(String) }
fn transition(state: ParentState) -> bool { matches!(state, ParentState::Compacting | ParentState::SessionSwitching | ParentState::SessionShutdown) }
fn multiple_reason(teams: &[ActiveTeamSummary]) -> String { format!("Multiple teams are owned by the current session: {}. Pass team_run_id.", teams.iter().map(|team| format!("{} ('{}')",team.team_run_id,team.team_name)).collect::<Vec<_>>().join(", ")) }
pub fn create_lead_poller_lifecycle(deps: LeadPollerLifecycleDeps) -> Arc<LeadPollerLifecycle> { let lifecycle = Arc::new(LeadPollerLifecycle { deps, pollers: Mutex::new(BTreeMap::new()), operation: Mutex::new(()), timer: Mutex::new(None), stopped: AtomicBool::new(false) }); lifecycle.schedule(); lifecycle }
impl LeadPollerLifecycle {
    fn schedule(self: &Arc<Self>) {
        if self.stopped.load(Ordering::SeqCst) { return; } let weak = Arc::downgrade(self);
        let timer = self.deps.timers.set(Box::new(move || { if let Some(lifecycle) = weak.upgrade() { if let Err(error) = lifecycle.tick() { (lifecycle.deps.on_error)(error); } lifecycle.schedule(); } }),1000);
        *self.timer.lock().unwrap_or_else(PoisonError::into_inner) = Some(timer);
    }
    fn synchronize(&self) -> Result<Vec<ActiveTeamSummary>, String> {
        let _operation = self.operation.lock().unwrap_or_else(PoisonError::into_inner);
        if self.stopped.load(Ordering::SeqCst) { return Ok(Vec::new()); }
        let session = (self.deps.session_id)(); let teams = (self.deps.list_teams)()?;
        let owned: Vec<_> = teams.into_iter().filter(|team| session.is_some() && team.lead_session_id == session).collect();
        let mut pollers = self.pollers.lock().unwrap_or_else(PoisonError::into_inner);
        let stale: Vec<_> = pollers.iter().filter(|(id,entry)| Some(&entry.owner) != session.as_ref() || !owned.iter().any(|team| &team.team_run_id == *id)).map(|(id,_)| id.clone()).collect();
        for id in stale { if let Some(entry) = pollers.remove(&id) { entry.poller.shutdown(); }
            if let Some(journal) = &self.deps.delivery_journal { journal.drop_team(&id); } }
        let Some(session) = session else { return Ok(owned); }; if (self.deps.session_file)().is_none() { return Ok(owned); }
        for team in &owned {
            if pollers.contains_key(&team.team_run_id) { continue; }
            let map = read_member_task_map(&(self.deps.runtime_dir)(&team.team_run_id));
            if self.stopped.load(Ordering::SeqCst) || (self.deps.session_id)().as_ref() != Some(&session) || (self.deps.session_file)().is_none() { break; }
            let append = self.deps.append_event.clone(); let file = self.deps.session_file.clone();
            let deps = LeadPollerDeps { team_run_id: team.team_run_id.clone(), config: self.deps.config.clone(), coordinator: self.deps.sink.clone(), delivery_journal: self.deps.delivery_journal.clone(), append_event: Some(Box::new(move |id,event| append(id,event))), event_task_id: Box::new(move |message| map.get(&message.from).cloned()), lead_session_file: Some(Box::new(move || file())) };
            let poller = match &self.deps.factory { Some(factory) => factory(deps), None => Arc::new(create_lead_poller(deps)) };
            pollers.insert(team.team_run_id.clone(), OwnedPoller { owner: session.clone(), poller });
        }
        Ok(owned)
    }
    pub fn tick(&self) -> Result<(), String> {
        if self.stopped.load(Ordering::SeqCst) || (self.deps.session_file)().is_none() { return Ok(()); }
        let owned = self.synchronize()?; if (self.deps.session_file)().is_none() || transition((self.deps.parent_state)()) { return Ok(()); }
        for team in owned { if let Some(poller) = self.resolve_lead_poller(&team.team_run_id) { poller.poll_once(None).map_err(|error| error.to_string())?; } } Ok(())
    }
    pub fn resolve_lead_poller(&self, run: &str) -> Option<Arc<dyn LeadPoller + Send + Sync>> {
        if self.stopped.load(Ordering::SeqCst) || (self.deps.session_file)().is_none() || transition((self.deps.parent_state)()) { return None; }
        self.pollers.lock().unwrap_or_else(PoisonError::into_inner).get(run).filter(|entry| Some(&entry.owner) == (self.deps.session_id)().as_ref()).map(|entry| entry.poller.clone())
    }
    pub fn resolve_team_run_id(&self, explicit: Option<&str>) -> Result<String,String> {
        let owned = self.synchronize()?;
        if let Some(id) = explicit { return if owned.iter().any(|team| team.team_run_id == id) { Ok(id.to_owned()) } else { Err(format!("Team {id} is not owned by the current session.")) }; }
        match owned.as_slice() { [team] => Ok(team.team_run_id.clone()), [] => Err("No active team is owned by the current session.".into()), _ => Err(multiple_reason(&owned)) }
    }
    pub fn resolve_default_team_run_id(&self) -> Result<DefaultTeamRunIdResolution,String> { let owned = self.synchronize()?; Ok(match owned.as_slice() { [team] => DefaultTeamRunIdResolution::Resolved(team.team_run_id.clone()), [] => DefaultTeamRunIdResolution::None, _ => DefaultTeamRunIdResolution::Ambiguous(multiple_reason(&owned)) }) }
    pub fn shutdown(&self) { if self.stopped.swap(true,Ordering::SeqCst) { return; }
        if let Some(timer) = self.timer.lock().unwrap_or_else(PoisonError::into_inner).take() { self.deps.timers.clear(timer); } let pollers = std::mem::take(&mut *self.pollers.lock().unwrap_or_else(PoisonError::into_inner)); for entry in pollers.into_values() { entry.poller.shutdown(); } }
}
