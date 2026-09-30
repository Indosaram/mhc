//! `tools/task/execute-continuation-scope.test.ts`

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use pretty_assertions::assert_eq;

use crate::host::HostError;
use crate::manager::concurrency::TaskConcurrencyConfig;
use crate::manager::execution_mode::ExecutionMode;
use crate::manager::types::{
    ChildPlanner, ManagedRunner, ManagedRunnerResult, ManagedRunners, ManagedStartSpec,
    ManagerConfig, ManagerStartSpec, ResolvedChildPlan, TaskManagerOptions,
};
use crate::manager::{
    ManagedChildHandle, ManagedChildListener, TaskManager, Unsubscribe, create_task_manager,
};
use crate::runners::RunnerOutcome;
use crate::store::{StateDirConfig, TaskRecordStore};
use crate::tools::task::execute::build_task_execute;
use crate::tools::task::execute_single::TaskExecuteDeps;
use crate::tools::task::execute_spec::TaskToolDeps;
use crate::tools::task::foreground_wait::{ForegroundWaitOptions, TaskToolContext};
use crate::tools::task::types::{SkillResolution, TaskToolMode};
use crate::tools::task::validation::SpawnParamsInput;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Minimal port of the manager-fakes scripted handle: records follow-ups, settles on demand.
struct FakeHandle {
    task_id: String,
    follow_up_calls: Mutex<Vec<String>>,
    outcomes: Mutex<Receiver<RunnerOutcome>>,
    _settle: Mutex<Sender<RunnerOutcome>>,
}

impl FakeHandle {
    fn new(task_id: &str) -> Arc<Self> {
        let (sender, receiver) = channel();
        Arc::new(Self {
            task_id: task_id.to_string(),
            follow_up_calls: Mutex::new(Vec::new()),
            outcomes: Mutex::new(receiver),
            _settle: Mutex::new(sender),
        })
    }

    fn follow_up_calls(&self) -> Vec<String> {
        lock(&self.follow_up_calls).clone()
    }
}

impl ManagedChildHandle for FakeHandle {
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

    fn follow_up(&self, text: &str) -> Result<(), HostError> {
        lock(&self.follow_up_calls).push(text.to_string());
        Ok(())
    }

    fn abort(&self) -> Result<(), HostError> {
        Ok(())
    }

    fn subscribe(&self, _listener: ManagedChildListener) -> Unsubscribe {
        Box::new(|| {})
    }

    fn wait_for_outcome(&self) -> RunnerOutcome {
        lock(&self.outcomes)
            .recv()
            .unwrap_or(RunnerOutcome::Cancelled)
    }

    fn last_assistant_text(&self) -> Option<String> {
        None
    }

    fn dispose(&self) -> Result<(), HostError> {
        Ok(())
    }
}

#[derive(Default)]
struct FakeRunner {
    handles: Mutex<HashMap<String, Arc<FakeHandle>>>,
}

impl FakeRunner {
    fn handle(&self, task_id: &str) -> Option<Arc<FakeHandle>> {
        lock(&self.handles).get(task_id).cloned()
    }
}

impl ManagedRunner for FakeRunner {
    fn start(&self, spec: &ManagedStartSpec) -> ManagedRunnerResult {
        let fake = FakeHandle::new(&spec.task_id);
        lock(&self.handles).insert(spec.task_id.clone(), Arc::clone(&fake));
        Ok(fake)
    }
}

fn planner() -> ChildPlanner {
    Arc::new(|spec: &ManagerStartSpec| {
        Ok(ResolvedChildPlan {
            model: spec
                .model
                .clone()
                .unwrap_or_else(|| "anthropic/claude".to_string()),
            category: spec.category.clone(),
            agent_type: spec.subagent_type.clone(),
            ..ResolvedChildPlan::default()
        })
    })
}

fn make_manager(dir: &tempfile::TempDir) -> (TaskManager, Arc<FakeRunner>) {
    let store = TaskRecordStore::new(&StateDirConfig {
        project_dir: dir.path().to_path_buf(),
        task_state_dir: None,
    });
    let in_process = Arc::new(FakeRunner::default());
    let process = Arc::new(FakeRunner::default());
    let mut options = TaskManagerOptions::new(
        store,
        ManagedRunners {
            in_process: Arc::clone(&in_process) as Arc<dyn ManagedRunner>,
            process: process as Arc<dyn ManagedRunner>,
        },
        planner(),
        dir.path().to_string_lossy().into_owned(),
    );
    options.config = ManagerConfig {
        concurrency: TaskConcurrencyConfig {
            default_concurrency: Some(5),
            provider_concurrency: None,
            model_concurrency: None,
        },
        max_depth: 1,
        default_execution_mode: ExecutionMode::InProcess,
    };
    (create_task_manager(options), in_process)
}

fn ctx_for(session_id: &str) -> TaskToolContext {
    TaskToolContext {
        cwd: "/work/project".to_string(),
        session_id: session_id.to_string(),
        get_prompt_cache_safe_wait_seconds: None,
    }
}

fn tool_deps() -> TaskToolDeps {
    TaskToolDeps {
        omo_config: Default::default(),
        agents: Default::default(),
        load_skills: Some(Arc::new(|_: &[String], _: &str| {
            SkillResolution::default()
        })),
        resolve_ancestry: None,
    }
}

fn params(prompt: &str) -> SpawnParamsInput {
    SpawnParamsInput {
        prompt: Some(prompt.to_string()),
        category: Some("quick".to_string()),
        run_in_background: Some(true),
        ..SpawnParamsInput::default()
    }
}

#[test]
fn given_a_real_manager_when_the_task_tool_executes_in_two_sessions_then_both_calls_spawn_children()
 {
    let dir = tempfile::tempdir().expect("tempdir");
    let (manager, in_process) = make_manager(&dir);
    let tool = tool_deps();
    let policy: TaskToolDeps = Default::default();
    let execute = build_task_execute(
        TaskExecuteDeps {
            manager: &manager,
            tool: &tool,
            policy: &policy,
        },
        ForegroundWaitOptions::default(),
    );

    let first = execute
        .execute("spawn-A", &params("own work"), None, None, &ctx_for("session-A"))
        .expect("first spawn");
    let second = execute
        .execute("spawn-B", &params("more work"), None, None, &ctx_for("session-B"))
        .expect("second spawn");

    assert_eq!(first.details.mode, TaskToolMode::Spawn);
    assert_eq!(second.details.mode, TaskToolMode::Spawn);
    assert_ne!(first.details.task_id, second.details.task_id);
    assert_eq!(
        in_process
            .handle(&first.details.task_id)
            .map(|handle| handle.follow_up_calls())
            .unwrap_or_default(),
        Vec::<String>::new()
    );
    assert_eq!(
        in_process
            .handle(&second.details.task_id)
            .map(|handle| handle.follow_up_calls())
            .unwrap_or_default(),
        Vec::<String>::new()
    );
}
