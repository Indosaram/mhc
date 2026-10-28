//! The task manager: starts children in the foreground or background under a concurrency limit,
//! persists their state, waits for and retrieves results, cancels, and resumes a finished task in
//! its own session. Design ported from the oh-my-pi task model (executor + async job delivery +
//! `parallel.ts` semaphore + `restore.ts` follow-up turns), adapted to process children.
use crate::agents::AgentDefinition;
use crate::ids::{IdAllocator, name_from_task};
use crate::isolation::{TaskObserver, WorkspaceIsolation};
use crate::models::ModelChoice;
use crate::record::{PendingMessage, StateDir, TaskRecord, TaskStatus, now_ms, pid_alive, reconcile};
use crate::runner::{CancelToken, RunOutcome, RunSpec, TaskRunner};
use crate::settings::TaskSettings;
use crate::summary::format_result;
use maho_lsp::hooks::{HookChain, Mutation};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Follow-up message of a resume that names none.
pub const DEFAULT_RESUME_PROMPT: &str =
    "Continue the task where you stopped and finish it. Reply with the final result.";

/// Called with the record and its `<task-result>` envelope when a background run settles.
pub type SettledCallback = Arc<dyn Fn(&TaskRecord, &str) + Send + Sync>;

pub struct TaskManagerOptions {
    pub state: StateDir,
    pub settings: TaskSettings,
    pub runner: Arc<dyn TaskRunner>,
    pub isolation: Option<Arc<dyn WorkspaceIsolation>>,
    pub observers: Vec<Arc<dyn TaskObserver>>,
    /// Post-run hooks over the files a child changed (e.g. LSP diagnostics).
    pub hooks: HookChain,
    /// Depth of the process that owns this manager.
    pub depth: u32,
}

/// A new task.
#[derive(Clone, Debug)]
pub struct SpawnRequest {
    pub agent: AgentDefinition,
    pub task: String,
    /// Shared background prepended to the task.
    pub context: Option<String>,
    pub name: Option<String>,
    pub choice: ModelChoice,
    pub cwd: PathBuf,
    pub background: bool,
    pub isolated: bool,
    /// Team (or other grouping) recorded on the task.
    pub group: Option<String>,
    /// Pane preference recorded on the task (`None` = observer default).
    pub pane: Option<bool>,
    /// Skip the background-delivery callback for this task.
    pub quiet: bool,
    /// Session that spawns the task (recorded for ownership checks).
    pub owner_session: Option<String>,
    /// Root session of the spawning session.
    pub root_session: Option<String>,
}

struct Live {
    cancel: CancelToken,
    done: tokio::sync::watch::Receiver<bool>,
    /// Distinguishes launches of the same id (a settled run must not remove its successor).
    generation: u64,
}

static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A steering delivery written into a running child's inbox, kept until the run settles.
#[derive(Clone, Debug)]
pub(crate) struct WrittenSteer {
    pub path: PathBuf,
    pub message: PendingMessage,
}

pub struct TaskManager {
    options: TaskManagerOptions,
    ids: Mutex<IdAllocator>,
    live: Mutex<HashMap<String, Live>>,
    /// Serializes record writes so the pending list is never lost to a concurrent save.
    io: Mutex<()>,
    /// Tasks with a `task_send` or cancel in progress.
    busy: Mutex<std::collections::HashSet<String>>,
    /// Live tasks being cancelled, with the reason to record.
    cancelling: Mutex<HashMap<String, Option<String>>>,
    /// Steering deliveries written per task during its current run.
    steers: Mutex<HashMap<String, Vec<WrittenSteer>>>,
    steer: Mutex<Option<Arc<dyn crate::control::SteerChannel>>>,
    slots: Option<Arc<tokio::sync::Semaphore>>,
    on_settled: Mutex<Option<SettledCallback>>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The child's user message: the shared context (when any) and the task.
pub fn compose_prompt(context: Option<&str>, task: &str) -> String {
    match context.map(str::trim).filter(|c| !c.is_empty()) {
        Some(context) => format!("# Context\n{context}\n\n# Task\n{}", task.trim()),
        None => task.trim().to_owned(),
    }
}

async fn git_status(cwd: &Path) -> Option<BTreeSet<String>> {
    let output = tokio::process::Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=all"])
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .ok()?;
    output.status.success().then(|| {
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::to_owned)
            .collect()
    })
}

fn changed_files(before: &BTreeSet<String>, after: &BTreeSet<String>) -> Vec<String> {
    after
        .difference(before)
        .filter_map(|line| line.get(3..))
        .map(|path| match path.split_once(" -> ") {
            Some((_, renamed)) => renamed.to_owned(),
            None => path.to_owned(),
        })
        .collect()
}

impl TaskManager {
    pub fn new(options: TaskManagerOptions) -> Arc<Self> {
        let slots = (options.settings.concurrency > 0)
            .then(|| Arc::new(tokio::sync::Semaphore::new(options.settings.concurrency)));
        let ids = IdAllocator::seeded_from(&options.state.root);
        Arc::new(Self {
            options,
            ids: Mutex::new(ids),
            live: Mutex::default(),
            io: Mutex::default(),
            busy: Mutex::default(),
            cancelling: Mutex::default(),
            steers: Mutex::default(),
            steer: Mutex::default(),
            slots,
            on_settled: Mutex::default(),
        })
    }

    /// Sets the channel that steers running children (`None` = sends to a running task are
    /// queued as a follow-up run).
    pub fn set_steer_channel(&self, channel: Option<Arc<dyn crate::control::SteerChannel>>) {
        *lock(&self.steer) = channel;
    }

    pub(crate) fn steer_channel(&self) -> Option<Arc<dyn crate::control::SteerChannel>> {
        lock(&self.steer).clone()
    }

    pub(crate) fn busy(&self) -> &Mutex<std::collections::HashSet<String>> {
        &self.busy
    }

    pub(crate) fn cancelling(&self) -> &Mutex<HashMap<String, Option<String>>> {
        &self.cancelling
    }

    pub(crate) fn steers(&self) -> &Mutex<HashMap<String, Vec<WrittenSteer>>> {
        &self.steers
    }

    pub(crate) fn live_here(&self, id: &str) -> bool {
        self.is_live(id)
    }

    pub(crate) fn cancel_token(&self, id: &str) -> Option<CancelToken> {
        lock(&self.live).get(id).map(|live| live.cancel.clone())
    }

    /// Saves `record`, keeping the pending list stored on disk (the pending list is only changed
    /// through [`TaskManager::update`]).
    fn persist(&self, record: &TaskRecord) {
        let _io = lock(&self.io);
        let mut record = record.clone();
        if let Some(stored) = self.options.state.load(&record.id) {
            record.pending_messages = stored.pending_messages;
        }
        let _ = self.options.state.save(&record);
    }

    /// Loads, changes and saves the record of `id` under the record lock.
    ///
    /// # Errors
    /// Unknown id or a write error.
    pub(crate) fn update<T>(
        &self,
        id: &str,
        change: impl FnOnce(&mut TaskRecord) -> T,
    ) -> Result<(TaskRecord, T), String> {
        let _io = lock(&self.io);
        let mut record = self
            .options
            .state
            .load(id)
            .ok_or_else(|| format!("Unknown task \"{id}\""))?;
        let value = change(&mut record);
        self.options
            .state
            .save(&record)
            .map_err(|error| format!("cannot save the task record: {error}"))?;
        Ok((record, value))
    }

    pub fn state(&self) -> &StateDir {
        &self.options.state
    }

    pub fn settings(&self) -> &TaskSettings {
        &self.options.settings
    }

    /// Sets the delivery callback for background results.
    pub fn set_on_settled(&self, callback: Option<SettledCallback>) {
        *lock(&self.on_settled) = callback;
    }

    fn is_live(&self, id: &str) -> bool {
        lock(&self.live).contains_key(id)
    }

    /// The record of `id`, reconciled against live processes.
    pub fn get(&self, id: &str) -> Option<TaskRecord> {
        // A task saves its final record before leaving `live`: checking before the load keeps a
        // concurrently finishing task from being overwritten as interrupted, and checking after
        // keeps the original guard for one that starts in between.
        let live_before = self.is_live(id);
        let mut record = self.options.state.load(id)?;
        if reconcile(&mut record, live_before || self.is_live(id)) {
            self.persist(&record);
        }
        Some(record)
    }

    /// Every task of this state directory, oldest first.
    pub fn list(&self) -> Vec<TaskRecord> {
        let live: std::collections::HashSet<String> = lock(&self.live).keys().cloned().collect();
        self.options
            .state
            .load_all()
            .into_iter()
            .map(|mut record| {
                let owned = live.contains(&record.id) || self.is_live(&record.id);
                if reconcile(&mut record, owned) {
                    self.persist(&record);
                }
                record
            })
            .collect()
    }

    /// The `<task-result>` envelope of `id` (current state).
    pub fn result_text(&self, id: &str) -> Option<String> {
        let record = self.get(id)?;
        Some(self.envelope(&record))
    }

    fn envelope(&self, record: &TaskRecord) -> String {
        let state = &self.options.state;
        format_result(
            record,
            &state.read_output(&record.id),
            &state.read_log_tail(&record.id, 4_000),
            &state.output_path(&record.id).to_string_lossy(),
        )
    }

    /// Starts a task; returns its record (queued or running). The run continues on the tokio
    /// runtime.
    ///
    /// # Errors
    /// Isolation requested without a backend, or the state directory cannot be written.
    pub fn start(self: &Arc<Self>, request: SpawnRequest) -> Result<TaskRecord, String> {
        if request.isolated && self.options.isolation.is_none() {
            return Err(
                "isolated tasks need a workspace isolation backend, which this build does not provide"
                    .to_owned(),
            );
        }
        let base = request
            .name
            .clone()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| name_from_task(&request.task));
        let id = lock(&self.ids).allocate(&base);
        let state = &self.options.state;
        std::fs::create_dir_all(&state.root)
            .map_err(|error| format!("cannot create {}: {error}", state.root.display()))?;
        let mut system = request.agent.system_prompt.clone();
        if let Some(append) = request.choice.prompt_append.as_deref() {
            system.push_str("\n\n");
            system.push_str(append);
        }
        if !system.trim().is_empty() {
            std::fs::write(state.prompt_path(&id), system.trim())
                .map_err(|error| format!("cannot write the task prompt: {error}"))?;
        }
        let mut exclude = request.agent.disallowed_tools.clone();
        for tool in &request.choice.disabled_tools {
            if !exclude.contains(tool) {
                exclude.push(tool.clone());
            }
        }
        let record = TaskRecord {
            id: id.clone(),
            agent: request.agent.name.clone(),
            description: request.task.lines().next().unwrap_or_default().chars().take(120).collect(),
            status: TaskStatus::Queued,
            model: request.choice.model.clone(),
            thinking: request.choice.thinking.clone(),
            model_source: request.choice.source.clone(),
            session_id: uuid::Uuid::new_v4().to_string(),
            cwd: request.cwd.clone(),
            background: request.background,
            pid: None,
            exit_code: None,
            error: None,
            runs: 1,
            started_at_ms: now_ms(),
            finished_at_ms: None,
            changed_files: Vec::new(),
            hook_notes: Vec::new(),
            tools: request.agent.tools.clone(),
            exclude_tools: exclude,
            isolated: request.isolated,
            group: request.group.clone(),
            pane: request.pane,
            quiet: request.quiet,
            name: request.name.clone().filter(|name| !name.trim().is_empty()),
            owner_session: request.owner_session.clone(),
            root_session: request.root_session.clone(),
            pending_messages: Vec::new(),
        };
        state
            .save(&record)
            .map_err(|error| format!("cannot save the task record: {error}"))?;
        let prompt = compose_prompt(request.context.as_deref(), &request.task);
        self.launch(record.clone(), prompt);
        Ok(record)
    }

    /// Runs a finished task again in its own session with a follow-up `prompt`.
    ///
    /// # Errors
    /// Unknown id, or the task is still active.
    pub fn resume(
        self: &Arc<Self>,
        id: &str,
        prompt: Option<&str>,
        background: bool,
    ) -> Result<TaskRecord, String> {
        let Some(record) = self.get(id) else {
            return Err(format!("Unknown task \"{id}\""));
        };
        if record.status.is_active() {
            return Err(format!(
                "Task \"{id}\" is still {}; wait for it or cancel it first",
                record.status.as_str()
            ));
        }
        if record.isolated && self.options.isolation.is_none() {
            return Err(format!("Task \"{id}\" ran isolated and cannot be resumed here"));
        }
        let prompt = prompt
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .unwrap_or(DEFAULT_RESUME_PROMPT)
            .to_owned();
        // Messages left over from an earlier parent process run first, then the new prompt.
        let (record, prompt) = self.update(&record.id, |record| {
            let mut texts: Vec<String> = std::mem::take(&mut record.pending_messages)
                .into_iter()
                .map(|message| message.text)
                .collect();
            texts.push(prompt);
            prepare_next_run(record, background);
            texts.join("\n\n")
        })?;
        self.launch(record.clone(), prompt);
        Ok(record)
    }

    fn launch(self: &Arc<Self>, record: TaskRecord, prompt: String) {
        let cancel = CancelToken::default();
        let (done_tx, done_rx) = tokio::sync::watch::channel(false);
        let generation = GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        lock(&self.live).insert(
            record.id.clone(),
            Live {
                cancel: cancel.clone(),
                done: done_rx,
                generation,
            },
        );
        let manager = Arc::clone(self);
        tokio::spawn(async move {
            let id = record.id.clone();
            let mut next = (record, prompt);
            let settled = loop {
                let settled = manager.execute(next.0, next.1, cancel.clone()).await;
                match manager.follow_up(&settled) {
                    Some(follow_up) => {
                        // Each settled run of a background task is reported, also when queued
                        // messages start another run right away.
                        manager.deliver(&settled);
                        next = follow_up;
                    }
                    None => break settled,
                }
            };
            lock(&manager.cancelling).remove(&id);
            {
                let mut live = lock(&manager.live);
                if live.get(&id).is_some_and(|entry| entry.generation == generation) {
                    live.remove(&id);
                }
            }
            let _ = done_tx.send(true);
            manager.deliver(&settled);
        });
    }

    fn deliver(&self, settled: &TaskRecord) {
        if settled.background && !settled.quiet {
            let callback = lock(&self.on_settled).clone();
            if let Some(callback) = callback {
                let text = self.envelope(settled);
                callback(settled, &text);
            }
        }
    }

    async fn execute(&self, mut record: TaskRecord, prompt: String, cancel: CancelToken) -> TaskRecord {
        let state = &self.options.state;
        let permit = match &self.slots {
            Some(slots) => {
                let slots = Arc::clone(slots);
                tokio::select! {
                    permit = slots.acquire_owned() => permit.ok(),
                    () = cancel.cancelled() => None,
                }
            }
            None => None,
        };
        if cancel.is_cancelled() {
            return self.finish(record, &RunOutcome { cancelled: true, ..RunOutcome::default() }, Vec::new());
        }
        let mut prompt = prompt;
        {
            // The first run of a queued task carries the messages sent while it waited.
            let _io = lock(&self.io);
            if let Some(stored) = state.load(&record.id) {
                let texts: Vec<String> = stored
                    .pending_messages
                    .into_iter()
                    .map(|message| message.text)
                    .collect();
                if !texts.is_empty() {
                    prompt = format!("{prompt}\n\n{}", texts.join("\n\n"));
                }
            }
            record.pending_messages.clear();
            record.status = TaskStatus::Running;
            let _ = state.save(&record);
        }
        for observer in &self.options.observers {
            observer.started_in(&record, state);
        }
        let mut notes = Vec::new();
        let mut cwd = record.cwd.clone();
        let mut workspace = None;
        if record.isolated
            && let Some(isolation) = &self.options.isolation
        {
            match isolation.prepare(&record.id, &record.cwd).await {
                Ok(prepared) => {
                    cwd.clone_from(&prepared.cwd);
                    workspace = Some(prepared);
                }
                Err(error) => {
                    let outcome = RunOutcome {
                        error: Some(format!("{} isolation failed: {error}", isolation.name())),
                        ..RunOutcome::default()
                    };
                    drop(permit);
                    return self.finish(record, &outcome, notes);
                }
            }
        }
        let track_changes = !self.options.hooks.is_empty();
        let before = if track_changes { git_status(&cwd).await } else { None };
        let prompt_file = state.prompt_path(&record.id);
        let spec = RunSpec {
            task_id: record.id.clone(),
            session_id: record.session_id.clone(),
            cwd: cwd.clone(),
            prompt,
            system_prompt_file: prompt_file.is_file().then_some(prompt_file),
            model: record.model.clone(),
            thinking: record.thinking.clone(),
            tools: record.tools.clone(),
            exclude_tools: record.exclude_tools.clone(),
            depth: self.options.depth + 1,
            output_path: state.output_path(&record.id),
            log_path: state.log_path(&record.id),
            sessions_dir: state.sessions_dir(),
            root_session: record.root_session.clone(),
        };
        let pid_slot: Arc<Mutex<Option<u32>>> = Arc::default();
        let pid_writer = Arc::clone(&pid_slot);
        let running = record.clone();
        let outcome = self
            .options
            .runner
            .run(
                &spec,
                cancel,
                Box::new(move |pid| {
                    *lock(&pid_writer) = pid;
                    let mut running = running;
                    running.pid = pid;
                    self.persist(&running);
                }),
            )
            .await;
        record.pid = *lock(&pid_slot);
        drop(permit);
        if let (Some(isolation), Some(workspace)) = (&self.options.isolation, &workspace) {
            match isolation.finish(workspace, outcome.succeeded()).await {
                Ok(Some(note)) => notes.push(note),
                Ok(None) => {}
                Err(error) => notes.push(format!("{} isolation: {error}", isolation.name())),
            }
        }
        if let (Some(before), Some(after)) = (before, if track_changes { git_status(&cwd).await } else { None }) {
            record.changed_files = changed_files(&before, &after);
            if !record.changed_files.is_empty() {
                let mutation = Mutation {
                    tool_name: "task".into(),
                    tool_call_id: record.id.clone(),
                    cwd: cwd.clone(),
                    files: record.changed_files.clone(),
                };
                notes.extend(self.options.hooks.run(&mutation).await);
            }
        }
        self.finish(record, &outcome, notes)
    }

    fn finish(&self, mut record: TaskRecord, outcome: &RunOutcome, notes: Vec<String>) -> TaskRecord {
        record.exit_code = outcome.exit_code;
        record.error.clone_from(&outcome.error);
        let cancel_reason = lock(&self.cancelling).get(&record.id).cloned();
        // A cancel recorded by another process (task-control.md §2) is final too.
        let cancelled_elsewhere = self
            .options
            .state
            .load(&record.id)
            .filter(|stored| stored.status == TaskStatus::Cancelled);
        if let Some(stored) = &cancelled_elsewhere {
            record.error.clone_from(&stored.error);
        }
        record.status = if outcome.cancelled || cancel_reason.is_some() || cancelled_elsewhere.is_some() {
            TaskStatus::Cancelled
        } else if outcome.succeeded() {
            TaskStatus::Completed
        } else {
            TaskStatus::Failed
        };
        if let Some(Some(reason)) = cancel_reason {
            record.error = Some(reason);
        }
        record.finished_at_ms = Some(now_ms());
        record.hook_notes = notes;
        if record.status == TaskStatus::Cancelled {
            // Cancel is final: the pending list is dropped with it.
            let _io = lock(&self.io);
            record.pending_messages.clear();
            let _ = self.options.state.save(&record);
        } else {
            self.persist(&record);
        }
        for observer in &self.options.observers {
            observer.finished_in(&record, &self.options.state);
        }
        record
    }

    /// After a run settled: reclaims steering deliveries the child did not drain, then starts the
    /// pending messages as one follow-up run (task-control.md §1, §1.1).
    fn follow_up(&self, settled: &TaskRecord) -> Option<(TaskRecord, String)> {
        let written = lock(&self.steers).remove(&settled.id).unwrap_or_default();
        let reclaimed: Vec<PendingMessage> = written
            .into_iter()
            .filter(|steer| std::fs::remove_file(&steer.path).is_ok())
            .map(|steer| steer.message)
            .collect();
        if settled.status == TaskStatus::Cancelled || lock(&self.cancelling).contains_key(&settled.id) {
            return None;
        }
        let (record, texts) = self
            .update(&settled.id, |record| {
                record.pending_messages.extend(reclaimed);
                record.pending_messages.sort_by_key(|message| message.created_at);
                if record.pending_messages.is_empty() {
                    return Vec::new();
                }
                let texts: Vec<String> = std::mem::take(&mut record.pending_messages)
                    .into_iter()
                    .map(|message| message.text)
                    .collect();
                let background = record.background;
                prepare_next_run(record, background);
                texts
            })
            .ok()?;
        (!texts.is_empty()).then(|| (record, texts.join("\n\n")))
    }

    /// Waits until `id` settles or `timeout` passes (`None` = no limit); returns the current
    /// record.
    pub async fn wait(&self, id: &str, timeout: Option<Duration>) -> Option<TaskRecord> {
        let receiver = lock(&self.live).get(id).map(|live| live.done.clone());
        if let Some(mut receiver) = receiver {
            let settled = receiver.wait_for(|done| *done);
            match timeout {
                Some(limit) => {
                    let _ = tokio::time::timeout(limit, settled).await;
                }
                None => {
                    let _ = settled.await;
                }
            }
        }
        self.get(id)
    }

    /// Starts a foreground task and waits for its envelope.
    ///
    /// # Errors
    /// See [`TaskManager::start`].
    pub async fn run(self: &Arc<Self>, request: SpawnRequest, cancel: &CancelToken) -> Result<String, String> {
        let record = self.start(request)?;
        tokio::select! {
            _ = self.wait(&record.id, None) => {}
            () = cancel.cancelled() => {
                self.cancel(&record.id);
                let _ = self.wait(&record.id, Some(Duration::from_secs(10))).await;
            }
        }
        self.result_text(&record.id)
            .ok_or_else(|| format!("task {} disappeared", record.id))
    }

    /// Cancels `id`: a run owned here is stopped through its token; an orphaned running child
    /// (owned by an ended process) is signalled directly. Returns a status message.
    pub fn cancel(&self, id: &str) -> String {
        if let Some(live) = lock(&self.live).get(id) {
            live.cancel.cancel();
            return format!("Cancelling task {id}");
        }
        let Some(record) = self.options.state.load(id) else {
            return format!("Unknown task \"{id}\"");
        };
        if !record.status.is_active() {
            return format!("Task {id} is already {}", record.status.as_str());
        }
        if let Some(pid) = record.pid.filter(|pid| pid_alive(*pid)) {
            terminate(pid);
        }
        let _ = self.update(id, |record| {
            record.status = TaskStatus::Cancelled;
            record.pending_messages.clear();
            record.finished_at_ms = Some(now_ms());
        });
        format!("Cancelled task {id}")
    }
}

/// Resets `record` for another run in the same session.
fn prepare_next_run(record: &mut TaskRecord, background: bool) {
    record.status = TaskStatus::Queued;
    record.runs += 1;
    record.background = background;
    record.pid = None;
    record.exit_code = None;
    record.error = None;
    record.started_at_ms = now_ms();
    record.finished_at_ms = None;
    record.changed_files.clear();
    record.hook_notes.clear();
}

#[cfg(unix)]
pub(crate) fn terminate(pid: u32) {
    if let Ok(raw) = i32::try_from(pid) {
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(raw),
            nix::sys::signal::Signal::SIGTERM,
        );
    }
}

#[cfg(windows)]
pub(crate) fn terminate(pid: u32) {
    let _ = std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn terminate(_pid: u32) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::bundled_agents;
    use crate::runner::BoxFuture;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Writes `echo:<prompt>` as the output; prompts starting with `fail` exit 3, `wait` blocks
    /// until cancelled.
    #[derive(Default)]
    struct FakeRunner {
        running: AtomicUsize,
        peak: AtomicUsize,
        specs: Mutex<Vec<RunSpec>>,
    }

    impl TaskRunner for FakeRunner {
        fn run<'a>(
            &'a self,
            spec: &'a RunSpec,
            cancel: CancelToken,
            on_start: Box<dyn FnOnce(Option<u32>) + Send + 'a>,
        ) -> BoxFuture<'a, RunOutcome> {
            Box::pin(async move {
                lock(&self.specs).push(spec.clone());
                on_start(Some(1));
                let now = self.running.fetch_add(1, Ordering::SeqCst) + 1;
                self.peak.fetch_max(now, Ordering::SeqCst);
                let outcome = if spec.prompt.starts_with("wait") {
                    cancel.cancelled().await;
                    RunOutcome { cancelled: true, ..RunOutcome::default() }
                } else {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    std::fs::write(&spec.output_path, format!("echo:{}", spec.prompt)).expect("out");
                    let code = if spec.prompt.starts_with("fail") { 3 } else { 0 };
                    RunOutcome { exit_code: Some(code), ..RunOutcome::default() }
                };
                self.running.fetch_sub(1, Ordering::SeqCst);
                outcome
            })
        }
    }

    fn manager(dir: &Path, concurrency: usize) -> (Arc<TaskManager>, Arc<FakeRunner>) {
        let runner = Arc::new(FakeRunner::default());
        let settings = TaskSettings {
            concurrency,
            ..TaskSettings::default()
        };
        let manager = TaskManager::new(TaskManagerOptions {
            state: StateDir::new(dir.join("state")),
            settings,
            runner: Arc::clone(&runner) as Arc<dyn TaskRunner>,
            isolation: None,
            observers: Vec::new(),
            hooks: HookChain::default(),
            depth: 0,
        });
        (manager, runner)
    }

    fn request(dir: &Path, task: &str, background: bool) -> SpawnRequest {
        SpawnRequest {
            agent: bundled_agents().remove(0),
            task: task.to_owned(),
            context: None,
            name: Some("Job".into()),
            choice: ModelChoice {
                model: Some("p/m".into()),
                ..ModelChoice::default()
            },
            cwd: dir.to_path_buf(),
            background,
            isolated: false,
            group: None,
            pane: None,
            quiet: false,
            owner_session: None,
            root_session: None,
        }
    }

    #[tokio::test]
    async fn foreground_run_returns_envelope() {
        let dir = tempfile::tempdir().expect("tmp");
        let (manager, runner) = manager(dir.path(), 2);
        let mut req = request(dir.path(), "do the thing", false);
        req.context = Some("shared".into());
        let text = manager.run(req, &CancelToken::default()).await.expect("run");
        assert!(text.contains("status=\"completed\""), "{text}");
        assert!(text.contains("echo:# Context\nshared\n\n# Task\ndo the thing"), "{text}");
        let spec = lock(&runner.specs)[0].clone();
        assert_eq!(spec.depth, 1);
        assert_eq!(spec.model.as_deref(), Some("p/m"));
        assert!(spec.system_prompt_file.is_some());
        let record = manager.get("Job").expect("record");
        assert_eq!(record.status, TaskStatus::Completed);
        assert_eq!(record.exit_code, Some(0));
    }

    #[tokio::test]
    async fn background_concurrency_delivery_and_resume() {
        let dir = tempfile::tempdir().expect("tmp");
        let (manager, runner) = manager(dir.path(), 2);
        let delivered: Arc<Mutex<Vec<String>>> = Arc::default();
        let sink = Arc::clone(&delivered);
        manager.set_on_settled(Some(Arc::new(move |record, text| {
            assert!(text.contains(&record.id));
            lock(&sink).push(record.id.clone());
        })));
        let ids: Vec<String> = (0..4)
            .map(|i| {
                manager
                    .start(request(dir.path(), &format!("job {i}"), true))
                    .expect("start")
                    .id
            })
            .collect();
        assert_eq!(ids, ["Job", "Job-2", "Job-3", "Job-4"]);
        for id in &ids {
            let record = manager.wait(id, Some(Duration::from_secs(5))).await.expect("record");
            assert_eq!(record.status, TaskStatus::Completed);
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(runner.peak.load(Ordering::SeqCst), 2);
        assert_eq!(lock(&delivered).len(), 4);

        let session = manager.get("Job").expect("record").session_id;
        let resumed = manager.resume("Job", Some("more"), false).expect("resume");
        assert_eq!(resumed.runs, 2);
        let record = manager.wait("Job", Some(Duration::from_secs(5))).await.expect("record");
        assert_eq!(record.status, TaskStatus::Completed);
        assert_eq!(record.session_id, session);
        assert_eq!(manager.state().read_output("Job"), "echo:more");
        let last = lock(&runner.specs).last().cloned().expect("spec");
        assert_eq!(last.session_id, session);
        assert_eq!(manager.list().len(), 4);
    }

    #[tokio::test]
    async fn failure_cancel_and_isolation() {
        let dir = tempfile::tempdir().expect("tmp");
        let (manager, _runner) = manager(dir.path(), 0);
        let text = manager
            .run(request(dir.path(), "fail now", false), &CancelToken::default())
            .await
            .expect("run");
        assert!(text.contains("failed (exit 3)"), "{text}");

        let record = manager.start(request(dir.path(), "wait forever", true)).expect("start");
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(manager.resume(&record.id, None, false).is_err());
        assert!(manager.cancel(&record.id).starts_with("Cancelling"));
        let record = manager.wait(&record.id, Some(Duration::from_secs(5))).await.expect("record");
        assert_eq!(record.status, TaskStatus::Cancelled);
        assert!(manager.cancel(&record.id).contains("already cancelled"));
        assert!(manager.cancel("nope").starts_with("Unknown"));

        let mut isolated = request(dir.path(), "x", false);
        isolated.isolated = true;
        assert!(manager.start(isolated).is_err());
    }

    #[tokio::test]
    async fn orphaned_records_become_interrupted() {
        let dir = tempfile::tempdir().expect("tmp");
        let (manager, _runner) = manager(dir.path(), 1);
        let mut record = crate::record::sample("Old");
        record.pid = None;
        manager.state().save(&record).expect("save");
        assert_eq!(manager.get("Old").expect("record").status, TaskStatus::Interrupted);
        let text = manager.result_text("Old").expect("text");
        assert!(text.contains("status=\"interrupted\""));
    }

    #[derive(Default)]
    struct Seen(Mutex<Vec<String>>);

    impl TaskObserver for Seen {
        fn started_in(&self, record: &TaskRecord, state: &StateDir) {
            let log = state.log_path(&record.id);
            lock(&self.0).push(format!("start {} {}", record.id, log.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
        }
        fn finished(&self, record: &TaskRecord) {
            lock(&self.0).push(format!("finish {} {}", record.id, record.status.as_str()));
        }
    }

    #[tokio::test]
    async fn observers_group_and_quiet_delivery() {
        let dir = tempfile::tempdir().expect("tmp");
        let seen = Arc::new(Seen::default());
        let manager = TaskManager::new(TaskManagerOptions {
            state: StateDir::new(dir.path().join("state")),
            settings: TaskSettings::default(),
            runner: Arc::new(FakeRunner::default()),
            isolation: None,
            observers: vec![Arc::clone(&seen) as Arc<dyn TaskObserver>],
            hooks: HookChain::default(),
            depth: 0,
        });
        let delivered: Arc<Mutex<Vec<String>>> = Arc::default();
        let sink = Arc::clone(&delivered);
        manager.set_on_settled(Some(Arc::new(move |record, _| lock(&sink).push(record.id.clone()))));
        let mut quiet = request(dir.path(), "q", true);
        quiet.name = Some("Quiet".into());
        quiet.group = Some("crew".into());
        quiet.quiet = true;
        let loud = request(dir.path(), "l", true);
        manager.start(quiet).expect("start");
        manager.start(loud).expect("start");
        for id in ["Quiet", "Job"] {
            manager.wait(id, Some(Duration::from_secs(5))).await.expect("record");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(*lock(&delivered), ["Job"]);
        let record = manager.get("Quiet").expect("record");
        assert_eq!(record.group.as_deref(), Some("crew"));
        assert!(record.quiet);
        let events = lock(&seen.0).clone();
        assert!(events.contains(&"start Quiet Quiet.log".to_owned()), "{events:?}");
        assert!(events.contains(&"finish Job completed".to_owned()), "{events:?}");
        let stored = std::fs::read_to_string(manager.state().record_path("Job")).expect("json");
        assert!(!stored.contains("\"quiet\"") && !stored.contains("\"group\""), "{stored}");
    }

    #[test]
    fn porcelain_diff() {
        let before: BTreeSet<String> = [" M a.rs".to_owned()].into();
        let after: BTreeSet<String> = [" M a.rs", "?? b.rs", "R  c.rs -> d.rs"]
            .map(str::to_owned)
            .into();
        assert_eq!(changed_files(&before, &after), ["b.rs", "d.rs"]);
    }
}
