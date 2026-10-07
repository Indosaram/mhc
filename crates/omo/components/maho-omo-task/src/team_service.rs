use std::{collections::BTreeSet, path::Path, sync::Arc};
use serde_json::Value;
use senpi_task::{manager::TaskManager, store::{StateDirConfig, TaskRecordStore, PersistedTaskEvent}, team::{member_projection::{refresh_team_member_statuses, RefreshTeamMemberStatusesDeps}, messaging::{types::{MessagingEngineDeps, SendTeamMessageInput, SendTeamMessageResult}, send::send_team_message, session_start_reconcile::{ReconcileTeamMailboxDeps, reconcile_team_mailbox_on_session_start}}, normalize::TEAM_LEAD_SENTINEL, runtime::{create_team, delete_team, TeamRuntimeError}, runtime_config::{TeamCoreConfig, TeamTaskBounds, to_team_core_config}, runtime_types::{CreateTeamDeps, CreateTeamResult, DeleteTeamDeps, DeleteTeamResult, TeamRuntimeManagerPort, TeamMemberDestructionPort, TeamMemberExtensionConfig}, shutdown::{request_shutdown, approve_shutdown, reject_shutdown, RequestShutdownDeps, ApproveShutdownDeps, ShutdownFailure}, storage::{team_storage_base_dir, resolve_team_runtime_dirs}, tasks::{TeamTasklistContext, CreateTeamTaskInput, TeamTaskFilter, create_team_task, list_team_tasks, claim_team_task, update_team_task_status, get_team_task}}, tools::team::types::*};
use team_core::{team_mailbox::list_unread_messages, team_registry::{discover_team_specs, get_task_claims_dir, load_team_spec, resolve_base_dir}, team_state_store::{list_active_teams, load_runtime_state, locks::detect_stale_lock}, team_tasklist::list_tasks, types::{RuntimeState, Task, TaskStatus, SpecSource}};
use crate::team_service_support::{build_member_ports, resolve_team_spec, make_shutdown_messenger, make_cancel_member_task};

pub type TeamEventAppender = Arc<dyn Fn(&str, PersistedTaskEvent) + Send + Sync>;
pub struct TeamServiceDeps {
    pub manager: Arc<TaskManager>,
    pub member_manager: Arc<dyn TeamRuntimeManagerPort + Send + Sync>,
    pub destruction: Arc<dyn TeamMemberDestructionPort + Send + Sync>,
    pub session_id: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    pub state_dir: StateDirConfig,
    pub bounds: TeamTaskBounds,
    pub omo_config: Value,
    pub agent_names: BTreeSet<String>,
    pub member_extension: TeamMemberExtensionConfig,
    pub append_task_event: Option<TeamEventAppender>,
    pub now: Option<Arc<dyn Fn() -> i64 + Send + Sync>>,
    pub new_message_id: Option<Arc<dyn Fn() -> String + Send + Sync>>,
}
pub struct TeamService { deps: TeamServiceDeps, config: TeamCoreConfig, append_event: TeamEventAppender }
struct MemberStatusView<'a>(&'a dyn TeamRuntimeManagerPort);
impl senpi_task::team::member_projection::MemberStatusPort for MemberStatusView<'_> {
    fn get(&self, task_id: &str) -> Option<senpi_task::team::member_projection::MemberTaskStatusView> {
        senpi_task::team::runtime_types::TeamMemberReadPort::get(self.0, task_id).map(|record| senpi_task::team::member_projection::MemberTaskStatusView { status: record.status, child_session_id: record.child_session_id })
    }
    fn get_resident_handle(&self, task_id: &str) -> Option<senpi_task::team::member_projection::ResidentSessionRef> { self.0.get_resident_handle(task_id) }
}
fn core_error(error: team_core::TeamCoreError) -> TeamToolServiceError { TeamToolServiceError::with_name(error.name(), error.to_string()) }
fn runtime_error(error: TeamRuntimeError) -> TeamToolServiceError {
    match error {
        TeamRuntimeError::Runtime(error) => TeamToolServiceError::with_code(error.name(), &error.message, error.code.as_str()),
        TeamRuntimeError::Core(error) => core_error(error),
        error => TeamToolServiceError::new(error.to_string()),
    }
}
fn shutdown_error(error: ShutdownFailure) -> TeamToolServiceError {
    match error { ShutdownFailure::Shutdown(error) => TeamToolServiceError::with_code(error.name(), &error.message, error.code.as_str()), ShutdownFailure::Core(error) => core_error(error), error => TeamToolServiceError::new(error.to_string()) }
}
pub fn assert_canonical_team_run_id(id: &str) -> TeamServiceResult<()> {
    let valid = id.len() == 36 && id.bytes().enumerate().all(|(index, byte)| match index { 8 | 13 | 18 | 23 => byte == b'-', 14 => byte == b'4', 19 => matches!(byte, b'8' | b'9' | b'a' | b'b'), _ => byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte) });
    if valid { Ok(()) } else { Err(TeamToolServiceError::new(format!("Team run id '{id}' must be a canonical lowercase UUID v4."))) }
}
pub fn create_team_service(deps: TeamServiceDeps) -> TeamServiceResult<TeamService> {
    let config = to_team_core_config(&deps.bounds, &team_storage_base_dir(&deps.state_dir).to_string_lossy()).map_err(TeamToolServiceError::new)?;
    let append_event = deps.append_task_event.clone().unwrap_or_else(|| {
        let store = TaskRecordStore::new(&deps.state_dir);
        Arc::new(move |id, event: PersistedTaskEvent| {
            if let Err(error) = store.append_event(id, &event) { eprintln!("omo-senpi task event append failed: taskId={id} eventType={} error={error}", event.event_type); }
        })
    });
    Ok(TeamService { deps, config, append_event })
}
impl TeamService {
    fn assert_owned(&self, run: &str) -> TeamServiceResult<()> {
        let Some(session) = (self.deps.session_id)() else { return Ok(()); };
        let rows = list_active_teams(&self.config).map_err(core_error)?;
        if rows.iter().find(|row| row.team_run_id == run).and_then(|row| row.lead_session_id.as_ref()).is_some_and(|owner| owner != &session) { return Err(TeamToolServiceError::new(format!("Team {run} is not owned by the current session."))); }
        Ok(())
    }
    fn task_context(&self, run: &str) -> TeamTasklistContext { TeamTasklistContext { team_run_id: run.to_owned(), config: self.config.clone() } }
    pub fn reconcile_mailbox(&self) { reconcile_team_mailbox_on_session_start(&ReconcileTeamMailboxDeps { state_dir: self.deps.state_dir.clone(), config: self.config.clone(), stale_ttl_ms: None, current_lead_session_id: (self.deps.session_id)() }); }
}
impl TeamToolsService for TeamService {
    fn create_team(&self, input: &CreateTeamToolInput) -> TeamServiceResult<CreateTeamResult> {
        let session = (self.deps.session_id)().filter(|id| !id.is_empty()).ok_or_else(|| TeamToolServiceError::new("team tools require an active lead session; none was captured yet"))?;
        let ports = build_member_ports(&self.deps.omo_config, &self.deps.agent_names);
        let resolved = resolve_team_spec(input.team_name.as_deref(), input.inline_spec.as_ref(), &ports, &self.deps.state_dir.project_dir, self.deps.omo_config.get("teams").and_then(Value::as_object)).map_err(|error| TeamToolServiceError::with_code(error.name(), &error.message, error.code.as_str()))?;
        create_team(&resolved.spec, resolved.source, &CreateTeamDeps { manager: self.deps.member_manager.clone(), state_dir: self.deps.state_dir.clone(), team_bounds: self.deps.bounds, lead_session_id: session, spawn_depth: 1, now: self.deps.now.clone(), member_extension: Some(self.deps.member_extension.clone()), write_member_map: None }).map_err(runtime_error)
    }
    fn delete_team(&self, input: &DeleteTeamToolInput) -> TeamServiceResult<DeleteTeamResult> {
        assert_canonical_team_run_id(&input.team_run_id)?; self.assert_owned(&input.team_run_id)?;
        delete_team(&input.team_run_id, &DeleteTeamDeps { manager: self.deps.member_manager.clone(), destruction: self.deps.destruction.clone(), state_dir: self.deps.state_dir.clone(), team_bounds: self.deps.bounds }).map_err(runtime_error)
    }
    fn send_message(&self, run: &str, input: &SendTeamMessageInput) -> TeamServiceResult<SendTeamMessageResult> {
        self.assert_owned(run)?;
        let state = load_runtime_state(run, &self.config).map_err(core_error)?;
        let append = self.append_event.clone();
        let now = self.deps.now.clone(); let message_id = self.deps.new_message_id.clone();
        send_team_message(input, &MessagingEngineDeps { team_run_id: run.to_owned(), state_dir: self.deps.state_dir.clone(), config: self.config.clone(), active_members: state.members.into_iter().map(|member| member.name).collect(), append_event: Some(Box::new(move |id, event| append(id, PersistedTaskEvent { event_type: event.event_type, payload: event.payload }))), now: now.map(|now| Box::new(move || now()) as Box<dyn Fn() -> i64 + Send + Sync>), new_message_id: message_id.map(|new_id| Box::new(move || new_id()) as Box<dyn Fn() -> String + Send + Sync>) }).map_err(|error| TeamToolServiceError::with_name(error.name(), error.to_string()))
    }
    fn status(&self, run: &str) -> TeamServiceResult<RuntimeState> {
        self.assert_owned(run)?;
        let dirs = resolve_team_runtime_dirs(&self.deps.state_dir, run).map_err(|error| TeamToolServiceError::new(error.to_string()))?;
        refresh_team_member_statuses(run, &RefreshTeamMemberStatusesDeps { manager: &MemberStatusView(self.deps.member_manager.as_ref()), config: &self.config, runtime_dir: &dirs.runtime_dir }).map_err(core_error)
    }
    fn list_teams(&self) -> TeamServiceResult<Vec<ActiveTeamSummary>> {
        Ok(list_active_teams(&self.config).map_err(core_error)?.into_iter().map(|row| ActiveTeamSummary { team_run_id: row.team_run_id, team_name: row.team_name, status: row.status.as_str().to_owned(), member_count: row.member_count, scope: match row.scope { SpecSource::Project => ActiveTeamScope::Project, SpecSource::User => ActiveTeamScope::User }, lead_session_id: row.lead_session_id }).collect())
    }
    fn create_task(&self, run: &str, input: &CreateTeamTaskServiceInput) -> TeamServiceResult<Task> {
        self.assert_owned(run)?;
        create_team_task(&self.task_context(run), CreateTeamTaskInput { subject: input.subject.clone(), description: input.description.clone(), status: input.status, owner: input.owner.clone(), blocked_by: input.blocked_by.clone(), active_form: None, blocks: None, metadata: None }).map_err(core_error)
    }
    fn list_tasks(&self, run: &str, filter: Option<&TeamTaskListFilter>) -> TeamServiceResult<Vec<Task>> { self.assert_owned(run)?; let filter = filter.map(|filter| TeamTaskFilter { status: filter.status, owner: filter.owner.clone() }); list_team_tasks(&self.task_context(run), filter.as_ref()).map_err(core_error) }
    fn update_task(&self, input: &UpdateTeamTaskServiceInput) -> TeamServiceResult<Task> {
        self.assert_owned(&input.team_run_id)?; let context = self.task_context(&input.team_run_id); let owner = input.owner.as_deref().unwrap_or(TEAM_LEAD_SENTINEL);
        match input.status { TaskStatus::Claimed => claim_team_task(&context, &input.task_id, owner), _ => update_team_task_status(&context, &input.task_id, input.status, owner) }.map_err(core_error)
    }
    fn get_task(&self, run: &str, id: &str) -> TeamServiceResult<Task> { self.assert_owned(run)?; get_team_task(&self.task_context(run), id).map_err(core_error) }
    fn request_shutdown(&self, run: &str, member: &str) -> TeamServiceResult<RuntimeState> {
        self.assert_owned(run)?; let send = make_shutdown_messenger(&self.deps.manager, &self.deps.state_dir, run); let now = || self.deps.now.as_ref().map_or_else(|| chrono::Utc::now().timestamp_millis(), |now| now());
        request_shutdown(run, member, &RequestShutdownDeps { config: &self.config, send_message: &send, now: self.deps.now.as_ref().map(|_| &now as &dyn Fn() -> i64) }).map_err(shutdown_error)
    }
    fn approve_shutdown(&self, run: &str, member: &str) -> TeamServiceResult<RuntimeState> {
        self.assert_owned(run)?; let send = make_shutdown_messenger(&self.deps.manager, &self.deps.state_dir, run); let cancel = make_cancel_member_task(&self.deps.manager, &self.deps.state_dir, run); let now = || self.deps.now.as_ref().map_or_else(|| chrono::Utc::now().timestamp_millis(), |now| now());
        approve_shutdown(run, member, &ApproveShutdownDeps { config: &self.config, send_message: &send, cancel_member_task: &cancel, now: self.deps.now.as_ref().map(|_| &now as &dyn Fn() -> i64) }).map_err(shutdown_error)
    }
    fn reject_shutdown(&self, run: &str, member: &str, reason: &str) -> TeamServiceResult<RuntimeState> {
        self.assert_owned(run)?; let send = make_shutdown_messenger(&self.deps.manager, &self.deps.state_dir, run); let now = || self.deps.now.as_ref().map_or_else(|| chrono::Utc::now().timestamp_millis(), |now| now());
        reject_shutdown(run, member, reason, &RequestShutdownDeps { config: &self.config, send_message: &send, now: self.deps.now.as_ref().map(|_| &now as &dyn Fn() -> i64) }).map_err(shutdown_error)
    }
    fn aggregate_status(&self, run: &str) -> TeamServiceResult<TeamStatus> {
        self.assert_owned(run)?;
        let state = load_runtime_state(run, &self.config).map_err(core_error)?;
        let members = state.members.iter().map(|member| {
            let unread_messages = list_unread_messages(run, &member.name, &self.config).map_or(0, |messages| messages.len());
            TeamStatusMember { name: member.name.clone(), session_id: member.session_id.clone(), status: member.status, color: member.color.clone(), worktree_path: member.worktree_path.clone(), unread_messages, pane_id: member.tmux_pane_id.clone() }
        }).collect();
        let tasks = list_tasks(run, &self.config, None).map_or_else(|_| TeamStatusTasks::default(), |tasks| count_tasks(&tasks));
        let base_dir = resolve_base_dir(&self.config);
        Ok(TeamStatus {
            team_name: state.team_name, team_run_id: state.team_run_id, status: state.status.as_str().to_owned(),
            lead_session_id: state.lead_session_id, created_at: state.created_at, members, tasks,
            shutdown_requests: state.shutdown_requests, concurrency: TeamStatusConcurrency::default(), bounds: state.bounds,
            stale_locks: stale_lock_paths(&base_dir, run),
        })
    }
    fn discover_team_specs(&self, project_root: &Path) -> TeamServiceResult<Vec<DiscoveredTeamSpec>> {
        Ok(discover_team_specs(&self.config, project_root).into_iter().map(|entry| DiscoveredTeamSpec {
            name: entry.name, scope: match entry.scope { SpecSource::Project => ActiveTeamScope::Project, SpecSource::User => ActiveTeamScope::User }, path: entry.path.to_string_lossy().into_owned(),
        }).collect())
    }
    fn load_team_spec_member_count(&self, name: &str, project_root: &Path) -> TeamServiceResult<usize> {
        Ok(load_team_spec(name, &self.config, project_root, None).map_err(core_error)?.members.len())
    }
    fn project_root(&self) -> std::path::PathBuf {
        self.deps.state_dir.project_dir.clone()
    }
}

fn count_tasks(tasks: &[Task]) -> TeamStatusTasks {
    let mut counts = TeamStatusTasks::default();
    for task in tasks {
        counts.total += 1;
        match task.status {
            TaskStatus::Pending => counts.pending += 1,
            TaskStatus::Claimed => counts.claimed += 1,
            TaskStatus::InProgress => counts.in_progress += 1,
            TaskStatus::Completed => counts.completed += 1,
            TaskStatus::Deleted => counts.deleted += 1,
        }
    }
    counts
}

fn stale_lock_paths(base_dir: &Path, run: &str) -> Vec<String> {
    const CLAIM_STALE_AFTER_MS: i64 = 300_000;
    let Ok(claims_dir) = get_task_claims_dir(base_dir, run) else { return Vec::new() };
    let Ok(entries) = std::fs::read_dir(&claims_dir) else { return Vec::new() };
    let mut paths: Vec<String> = entries.flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()) && entry.file_name().to_string_lossy().ends_with(".lock"))
        .map(|entry| entry.path())
        .filter(|path| detect_stale_lock(path, CLAIM_STALE_AFTER_MS))
        .map(|path| path.to_string_lossy().into_owned())
        .collect();
    paths.sort();
    paths
}
