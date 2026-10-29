//! `tools/team/lifecycle-collision.test.ts`

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use pretty_assertions::assert_eq;
use serde_json::json;

use crate::host::HostError;
use crate::manager::concurrency::TaskConcurrencyConfig;
use crate::manager::execution_mode::ExecutionMode;
use crate::manager::types::{
    ChildPlanner, ManagedRunner, ManagedRunnerResult, ManagedRunners, ManagedStartSpec, ManagerConfig,
    ManagerStartSpec, ResolvedChildPlan, StartResult, TaskManagerOptions,
};
use crate::manager::{ManagedChildHandle, ManagedChildListener, TaskManager, Unsubscribe, create_task_manager};
use crate::runners::RunnerOutcome;
use crate::state::{ResidencyState, TaskStatus};
use crate::store::{StateDirConfig, TaskRecordStore};
use crate::team::member_projection::ResidentSessionRef;
use crate::team::normalize::normalize_senpi_team_spec;
use crate::team::registry::TeamSpecSource;
use crate::team::runtime::{TeamRuntimeError, create_team};
use crate::team::runtime_config::TeamTaskBounds;
use crate::team::runtime_types::{
    CreateTeamDeps, TeamCancelOutcome, TeamMemberCancelPort, TeamMemberReadPort, TeamMemberStartSpec,
    TeamMemberTaskRecord, TeamRuntimeManagerPort, TeamStartResult, TeamStartedMember,
};
use crate::tools::team::lifecycle::{
    TeamCreateDetails, TeamCreateInput, run_team_create, runtime_error_to_service_error,
    spec_error_to_service_error,
};
use crate::tools::team::team_tool_fakes::{FakeTeamServiceOverrides, create_fake_team_service};
use crate::tools::team::types::TeamToolServiceError;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

// A child handle that stays running: its outcome channel never settles while the handle lives.
struct IdleHandle {
    task_id: String,
    outcomes: Mutex<Receiver<RunnerOutcome>>,
    _settle: Mutex<Sender<RunnerOutcome>>,
}

impl ManagedChildHandle for IdleHandle {
    fn task_id(&self) -> &str {
        &self.task_id
    }

    fn session_id(&self) -> Option<String> {
        Some(format!("sess-{}", self.task_id))
    }

    fn pid(&self) -> Option<i64> {
        None
    }

    fn steer(&self, _text: &str) -> Result<(), HostError> {
        Ok(())
    }

    fn follow_up(&self, _text: &str) -> Result<(), HostError> {
        Ok(())
    }

    fn abort(&self) -> Result<(), HostError> {
        Ok(())
    }

    fn subscribe(&self, _listener: ManagedChildListener) -> Unsubscribe {
        Box::new(|| {})
    }

    fn wait_for_outcome(&self) -> RunnerOutcome {
        lock(&self.outcomes).recv().unwrap_or(RunnerOutcome::Cancelled)
    }

    fn last_assistant_text(&self) -> Option<String> {
        None
    }

    fn dispose(&self) -> Result<(), HostError> {
        Ok(())
    }
}

#[derive(Default)]
struct IdleRunner;

impl ManagedRunner for IdleRunner {
    fn start(&self, spec: &ManagedStartSpec) -> ManagedRunnerResult {
        let (sender, receiver) = channel();
        Ok(Arc::new(IdleHandle {
            task_id: spec.task_id.clone(),
            outcomes: Mutex::new(receiver),
            _settle: Mutex::new(sender),
        }))
    }
}

// Team runtime port over a real TaskManager: start/get/resident handle go through the manager.
struct ManagerPort {
    manager: TaskManager,
    specs: Mutex<HashMap<String, TeamMemberStartSpec>>,
}

impl TeamMemberReadPort for ManagerPort {
    fn get(&self, task_id: &str) -> Option<TeamMemberTaskRecord> {
        let record = self.manager.get(task_id)?;
        let spec = lock(&self.specs).get(task_id).cloned()?;
        let child_session_id = self
            .manager
            .get_resident_handle(task_id)
            .and_then(|handle| handle.session_id());
        Some(TeamMemberTaskRecord {
            task_id: task_id.to_string(),
            status: record.status,
            residency_state: ResidencyState::parse("resident").expect("known residency state"),
            created_at: String::new(),
            updated_at: String::new(),
            parent_session_id: spec.parent_session_id.clone(),
            root_session_id: spec
                .root_session_id
                .clone()
                .unwrap_or_else(|| spec.parent_session_id.clone()),
            depth: spec.depth,
            execution_mode: spec.execution_mode.unwrap_or(ExecutionMode::InProcess),
            model: spec.model.clone().unwrap_or_default(),
            child_session_id,
            resolved_model: None,
            name: spec.name.clone(),
            category: spec.category.clone(),
            agent_type: spec.subagent_type.clone(),
        })
    }
}

impl TeamMemberCancelPort for ManagerPort {
    fn cancel_task(&self, id_or_name: &str, _reason: Option<&str>) -> TeamCancelOutcome {
        TeamCancelOutcome::NotFound {
            reason: format!("unexpected cancel of {id_or_name}"),
        }
    }
}

impl TeamRuntimeManagerPort for ManagerPort {
    fn start(&self, spec: &TeamMemberStartSpec) -> Result<TeamStartResult, String> {
        let manager_spec = ManagerStartSpec::from(spec);
        match self.manager.start(&manager_spec) {
            StartResult::Started(task) => {
                let task_id = task.task_id.clone();
                lock(&self.specs).insert(task_id.clone(), spec.clone());
                let status = self
                    .manager
                    .get(&task_id)
                    .map(|record| record.status)
                    .unwrap_or_else(|| TaskStatus::parse("running").expect("known task status"));
                Ok(TeamStartResult::Started(TeamStartedMember {
                    name: spec.name.clone().unwrap_or_else(|| task_id.clone()),
                    task_id,
                    status,
                    resolved_model: None,
                }))
            }
            other => Ok(TeamStartResult::Rejected {
                kind: other.kind().to_string(),
                reason: format!("{other:?}"),
            }),
        }
    }

    fn get_resident_handle(&self, task_id: &str) -> Option<ResidentSessionRef> {
        self.manager
            .get_resident_handle(task_id)
            .map(|handle| ResidentSessionRef {
                session_id: handle.session_id(),
            })
    }
}

fn state_dir_config(project: &std::path::Path) -> StateDirConfig {
    StateDirConfig {
        project_dir: project.to_path_buf(),
        task_state_dir: None,
    }
}

fn build_manager(project: &std::path::Path) -> TaskManager {
    let store = TaskRecordStore::new(&state_dir_config(project));
    let planner: ChildPlanner = Arc::new(|spec: &ManagerStartSpec| {
        Ok(ResolvedChildPlan {
            model: spec.model.clone().unwrap_or_else(|| "anthropic/claude".to_string()),
            category: spec.category.clone(),
            agent_type: spec.subagent_type.clone(),
            ..ResolvedChildPlan::default()
        })
    });
    let mut options = TaskManagerOptions::new(
        store,
        ManagedRunners {
            in_process: Arc::new(IdleRunner) as Arc<dyn ManagedRunner>,
            process: Arc::new(IdleRunner) as Arc<dyn ManagedRunner>,
        },
        planner,
        project.to_string_lossy().into_owned(),
    );
    options.config = ManagerConfig {
        concurrency: TaskConcurrencyConfig {
            default_concurrency: Some(5),
            provider_concurrency: None,
            model_concurrency: None,
        },
        max_depth: 2,
        default_execution_mode: ExecutionMode::InProcess,
    };
    create_task_manager(options)
}

#[test]
fn given_a_first_save_collision_when_the_real_team_create_tool_creates_a_team_then_the_team_activates_without_a_member_rejection_or_raw_collision()
 {
    // given
    let temp = tempfile::tempdir().expect("tempdir");
    let project: PathBuf = temp.path().to_path_buf();
    let team_status: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let status_slot = Arc::clone(&team_status);
    let service = create_fake_team_service(FakeTeamServiceOverrides {
        create_team: Some(Box::new(move |input| {
            let Some(inline_spec) = &input.inline_spec else {
                return Err(TeamToolServiceError::new("expected inline team spec".to_string()));
            };
            let spec = normalize_senpi_team_spec(inline_spec, "collision-team", None)
                .map_err(|error| spec_error_to_service_error(&error))?;
            let manager = build_manager(&project);
            let deps = CreateTeamDeps {
                manager: Arc::new(ManagerPort {
                    manager,
                    specs: Mutex::new(HashMap::new()),
                }),
                state_dir: state_dir_config(&project),
                team_bounds: TeamTaskBounds {
                    max_members: 8,
                    max_parallel_members: 4,
                    max_wall_clock_minutes: 120,
                },
                lead_session_id: "lead-session".to_string(),
                spawn_depth: 1,
                now: None,
                member_extension: None,
                write_member_map: None,
            };
            let created = match create_team(&spec, TeamSpecSource::Project, &deps) {
                Ok(created) => created,
                Err(TeamRuntimeError::Runtime(error)) => return Err(runtime_error_to_service_error(&error)),
                Err(other) => return Err(TeamToolServiceError::new(other.to_string())),
            };
            *lock(&status_slot) = Some(created.runtime_state.status.as_str().to_string());
            Ok(created)
        })),
        ..FakeTeamServiceOverrides::default()
    });

    // when
    let result = run_team_create(
        &service,
        &TeamCreateInput {
            team_name: None,
            inline_spec: Some(json!({
                "name": "collision-team",
                "members": [{ "name": "alpha", "kind": "category", "category": "quick", "prompt": "work" }]
            })),
        },
    )
    .expect("team_create result");

    // then
    match &result.details {
        TeamCreateDetails::Created { members, .. } => {
            assert_eq!(members.len(), 1);
            assert_eq!(members[0].name, "alpha");
            assert_eq!(members[0].status, "running");
        }
        other => panic!("expected created, got {other:?}"),
    }
    assert_eq!(lock(&team_status).clone(), Some("active".to_string()));
    let rendered = format!("{result:?}");
    assert!(!rendered.contains("member_start_rejected"));
    assert!(!rendered.contains("already exists"));
}
