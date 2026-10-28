//! The `maho-task` integration: [`TaskExecutor`] runs each node as a background delegated task
//! (same agent, category and model rules as the `task` tool), and [`WorkflowAddon`] contributes
//! the `workflow` tool and `/dag` to the task extension, so both share its depth gate, state
//! directory and concurrency limit.
use crate::dag;
use crate::executor::{BoxFuture, ChildOutcome, ChildProbe, NodeExecutor, NodeRequest, SendOutcome, StartError};
use crate::scheduler::{SchedulerOptions, WorkflowScheduler};
use crate::settings::DagSettings;
use crate::skills::DirSkillResolver;
use crate::state::codes;
use crate::store::WorkflowStore;
use crate::tool::{self, Caller, TOOL_NAME};
use crate::lock;
use maho_ext_api::{ToolContent, ToolResult};
use maho_task::{AddonCommand, AddonTool, TaskAddon, TaskManager, TaskRecord, TaskScope, TaskStatus};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

/// How often a re-attached child owned by an ended process is checked for exit. Children started
/// here are awaited through the task manager without polling.
const ORPHAN_CHECK: Duration = Duration::from_secs(2);

/// Runs nodes as `maho-task` background tasks.
pub struct TaskExecutor {
    scope: TaskScope,
}

impl TaskExecutor {
    pub fn new(scope: TaskScope) -> Self {
        Self { scope }
    }

    fn manager(&self) -> Arc<TaskManager> {
        self.scope.manager()
    }
}

/// The child outcome of a settled task record.
pub fn outcome_of(record: &TaskRecord, output: String) -> ChildOutcome {
    match record.status {
        TaskStatus::Completed => ChildOutcome::Completed { output },
        TaskStatus::Cancelled => ChildOutcome::Cancelled,
        TaskStatus::Interrupted => ChildOutcome::Interrupted {
            message: "the process that ran the child ended before it reported back".to_owned(),
        },
        // A spawn or wait error is the host's problem; a non-zero exit is the child's.
        TaskStatus::Failed => match &record.error {
            Some(error) => ChildOutcome::Failed { message: error.clone(), infrastructure: true },
            None => ChildOutcome::Failed {
                message: match record.exit_code {
                    Some(code) => format!("the child exited with code {code}"),
                    None => "the child ended without an exit code".to_owned(),
                },
                infrastructure: false,
            },
        },
        TaskStatus::Queued | TaskStatus::Running => ChildOutcome::Interrupted {
            message: "the child is still active".to_owned(),
        },
    }
}

impl NodeExecutor for TaskExecutor {
    fn start(&self, request: &NodeRequest) -> Result<String, StartError> {
        let mut item = serde_json::Map::new();
        item.insert("task".into(), json!(request.prompt));
        if let Some(category) = &request.category {
            item.insert("category".into(), json!(category));
        }
        if let Some(agent) = &request.subagent_type {
            item.insert("agent".into(), json!(agent));
        }
        if let Some(model) = &request.model {
            item.insert("model".into(), json!(model));
        }
        item.insert("name".into(), json!(format!("wf-{}", request.node_id)));
        let mut spawn = self
            .scope
            .request(&Value::Object(item), None)
            .map_err(|message| StartError { code: codes::PLAN_UNRESOLVED.to_owned(), message })?;
        spawn.quiet = true;
        spawn.group = Some(request.run_id.clone());
        self.manager()
            .start(spawn)
            .map(|record| record.id)
            .map_err(|message| StartError { code: codes::START_FAILED.to_owned(), message })
    }

    fn wait(&self, task_id: &str) -> BoxFuture<'static, ChildOutcome> {
        let manager = self.manager();
        let id = task_id.to_owned();
        Box::pin(async move {
            loop {
                let Some(record) = manager.wait(&id, None).await else {
                    return ChildOutcome::Failed {
                        message: format!("task {id} disappeared"),
                        infrastructure: true,
                    };
                };
                if record.status.is_active() {
                    tokio::time::sleep(ORPHAN_CHECK).await;
                    continue;
                }
                let output = manager.state().read_output(&id);
                return outcome_of(&record, output);
            }
        })
    }

    fn cancel(&self, task_id: &str) {
        self.manager().cancel(task_id);
    }

    fn send(&self, task_id: &str, message: &str) -> SendOutcome {
        let manager = self.manager();
        match manager.get(task_id) {
            // Children are non-interactive processes: a running one cannot take a message.
            Some(record) if record.status.is_active() => SendOutcome::NotContinuable,
            Some(_) => match manager.resume(task_id, Some(message), true) {
                Ok(_) => SendOutcome::FollowUp,
                Err(_) => SendOutcome::NotContinuable,
            },
            None => SendOutcome::NotContinuable,
        }
    }

    fn probe(&self, task_id: &str) -> ChildProbe {
        let manager = self.manager();
        match manager.get(task_id) {
            None => ChildProbe::Missing,
            Some(record) if record.status.is_active() => ChildProbe::Running,
            Some(record) if record.status == TaskStatus::Interrupted => ChildProbe::Lost,
            Some(record) => ChildProbe::Settled(outcome_of(&record, manager.state().read_output(task_id))),
        }
    }

    fn last_activity(&self, task_id: &str) -> Option<u64> {
        let state = self.manager().state().clone();
        [state.log_path(task_id), state.output_path(task_id)]
            .iter()
            .filter_map(|path| std::fs::metadata(path).and_then(|m| m.modified()).ok())
            .filter_map(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
            .max()
    }
}

static SCHEDULERS: Mutex<Option<HashMap<PathBuf, Arc<WorkflowScheduler>>>> = Mutex::new(None);

/// The `workflow` tool and `/dag` as a task addon.
#[derive(Clone, Debug, Default)]
pub struct WorkflowAddon {
    /// Extra skill roots searched for `load_skills` (after `<cwd>/.omo/skills` and
    /// `<cwd>/.agents/skills`), e.g. the agent dir's `skills/` and the bundled root.
    pub skill_roots: Vec<PathBuf>,
}

fn spawn_heartbeat(scheduler: &Arc<WorkflowScheduler>) {
    let Ok(runtime) = tokio::runtime::Handle::try_current() else { return };
    let weak: Weak<WorkflowScheduler> = Arc::downgrade(scheduler);
    let every = Duration::from_millis(scheduler.settings().heartbeat_ms.max(1));
    runtime.spawn(async move {
        let mut interval = tokio::time::interval(every);
        interval.tick().await;
        loop {
            interval.tick().await;
            let Some(scheduler) = weak.upgrade() else { return };
            if scheduler.is_stopped() {
                return;
            }
            scheduler.heartbeat();
        }
    });
}

impl WorkflowAddon {
    /// The scheduler of the calling session's task state directory; created on first use, which
    /// also recovers the session's orphaned runs and starts the retention sweep.
    pub fn scheduler(&self, scope: &TaskScope) -> Arc<WorkflowScheduler> {
        let root = scope.manager().state().root.clone();
        let mut map = lock(&SCHEDULERS);
        let map = map.get_or_insert_with(HashMap::new);
        if let Some(existing) = map.get(&root) {
            return Arc::clone(existing);
        }
        let mut roots = vec![scope.cwd().join(".omo").join("skills"), scope.cwd().join(".agents").join("skills")];
        roots.extend(self.skill_roots.iter().cloned());
        let scheduler = WorkflowScheduler::new(SchedulerOptions {
            store: WorkflowStore::in_state_dir(&root),
            settings: DagSettings::from_view(Some(scope.view())),
            executor: Arc::new(TaskExecutor::new(scope.clone())),
            skills: Arc::new(DirSkillResolver { roots }),
            retry_base: Duration::from_secs(1),
        });
        map.insert(root, Arc::clone(&scheduler));
        scheduler.recover(scope.session_id());
        spawn_heartbeat(&scheduler);
        let sweeper = Arc::clone(&scheduler);
        let sweep = move || {
            let (_, failures) = sweeper.sweep();
            for failure in failures {
                maho_ext_api::diagnostic(&format!("workflow retention: {failure}"));
            }
        };
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let _ = runtime.spawn_blocking(sweep);
        } else {
            let _ = std::thread::Builder::new().name("workflow-retention".into()).spawn(sweep);
        }
        scheduler
    }
}

fn caller(scope: &TaskScope) -> Caller {
    Caller::new(scope.session_id())
}

impl TaskAddon for WorkflowAddon {
    fn tools(&self) -> Vec<AddonTool> {
        let addon = self.clone();
        let handler: maho_task::AddonHandler = Arc::new(move |scope: TaskScope, params: Value, _cancel| {
                let addon = addon.clone();
                Box::pin(async move {
                    let scheduler = addon.scheduler(&scope);
                    let outcome = tool::run(&scheduler, &caller(&scope), &params, scope.notifier()).await;
                    Ok::<ToolResult, maho_ext_api::ToolError>(ToolResult {
                        content: vec![ToolContent::text(outcome.text)],
                        details: Some(outcome.details),
                        structured_content: None,
                        is_error: outcome.is_error.then_some(true),
                    })
                })
        });
        vec![AddonTool {
            name: TOOL_NAME.to_owned(),
            label: "Workflow".to_owned(),
            description: tool::DESCRIPTION.to_owned(),
            parameters: tool::parameters(),
            handler,
        }]
    }

    fn commands(&self) -> Vec<AddonCommand> {
        let addon = self.clone();
        vec![AddonCommand {
            name: "dag".to_owned(),
            description: "Show this session's workflow runs, or one run's waves (/dag <run id>)".to_owned(),
            handler: Arc::new(move |scope, raw| {
                let scheduler = addon.scheduler(&scope);
                let output = dag::render(&scheduler, &caller(&scope), &raw);
                if output.warning { format!("Warning: {}", output.text) } else { output.text }
            }),
        }]
    }
}
