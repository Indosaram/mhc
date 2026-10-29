//! The panel controller: state, refresh lifecycle and actions (spec 3, 5, 6, 7).
//!
//! The controller talks to the session only through [`PanelHost`], so the lifecycle can be driven
//! by the extension adapter in production and by a recording host in tests.

use crate::git::{self, ChangedFile, DiffRow};
use crate::goal::GoalReader;
use crate::layout::{self, MountMode};
use crate::links::Link;
use crate::sections::{self, MemoryInfo, PanelView, SessionFacts, Style, UsageWindow};
use crate::settings::Settings;
use crate::tasks::{self, TaskBoard, TaskUpdate};
use crate::tools::ToolLog;
use crate::viewer::{self, Viewer, ViewerRow};
use maho_ext_api::{ExecResult, NotificationType};
use serde_json::Value;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

/// A boxed future returned by the host.
pub type HostFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Why a host command did not produce a result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecError {
    /// The session has no command runner.
    Unavailable,
    /// The command could not be started or timed out.
    Failed(String),
}

/// The session seen by the panel.
pub trait PanelHost: Send + Sync {
    /// Milliseconds on the host clock.
    fn now_ms(&self) -> u64;
    /// Runs `git <args>` in `cwd` with a timeout.
    fn git(
        &self,
        args: Vec<String>,
        cwd: PathBuf,
        timeout_ms: u64,
    ) -> HostFuture<Result<ExecResult, ExecError>>;
    /// Requests a repaint.
    fn repaint(&self);
    /// Shows the panel (column or block above the editor) and installs the link handler.
    fn mount(&self, mode: MountMode);
    /// Removes the panel, restoring the host's original layout and link handler.
    fn unmount(&self);
    fn notify(&self, message: &str, kind: NotificationType);
    /// Opens a read-only viewer.
    fn open_viewer(&self, viewer: Viewer);
    /// Lets the user pick one option; `None` when dismissed.
    fn pick(&self, title: String, options: Vec<String>) -> HostFuture<Option<String>>;
    /// Debug-level log line.
    fn debug(&self, message: &str);
}

/// Reads the memory store status (`None` = nothing resolves).
pub type MemoryReader = Arc<dyn Fn() -> Option<MemoryInfo> + Send + Sync>;
/// Reads plan usage windows (`None` = missing credentials or a failed poll).
pub type UsageReader = Arc<dyn Fn() -> HostFuture<Option<Vec<UsageWindow>>> + Send + Sync>;

/// Session facts fixed at session start.
#[derive(Clone, Default)]
pub struct PanelEnv {
    pub cwd: PathBuf,
    pub home: Option<PathBuf>,
    pub agent_dir: PathBuf,
    /// `None` = no session id: no tasks are listed.
    pub session_id: Option<String>,
    pub task_dir: Option<PathBuf>,
    pub goal_file: Option<PathBuf>,
    /// Whether the host layout is fullscreen (column mode).
    pub fullscreen: bool,
    /// Whether clickable rows can be used (terminal links and a host link handler).
    pub links_supported: bool,
    pub memory: Option<MemoryReader>,
    pub usage: Option<UsageReader>,
}

/// Memory refresh throttle.
pub const MEMORY_THROTTLE: Duration = Duration::from_secs(5);
/// Repaint interval while tasks are active.
pub const TICK: Duration = Duration::from_secs(1);
/// Terminal height a viewer is created with; the host adapter replaces it with the real height.
pub const DEFAULT_TERMINAL_ROWS: usize = 24;

#[derive(Default)]
struct State {
    generation: u64,
    mounted: Option<MountMode>,
    mounted_at_ms: u64,
    facts: SessionFacts,
    goal: Option<crate::goal::Goal>,
    goal_reader: GoalReader,
    usage: Vec<UsageWindow>,
    board: TaskBoard,
    tools: ToolLog,
    files: Option<Vec<ChangedFile>>,
    repo_root: Option<PathBuf>,
    branch: Option<String>,
    memory: Option<MemoryInfo>,
    git_throttle: git::Throttle,
    memory_throttle: git::Throttle,
    ticker_running: bool,
    ticker_id: u64,
    git_runs: u64,
}

/// The side panel of one session.
pub struct Panel {
    host: Arc<dyn PanelHost>,
    settings: Settings,
    env: PanelEnv,
    state: Mutex<State>,
}

impl std::fmt::Debug for Panel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Panel")
            .field("settings", &self.settings)
            .finish_non_exhaustive()
    }
}

impl Panel {
    pub fn new(host: Arc<dyn PanelHost>, settings: Settings, env: PanelEnv) -> Arc<Self> {
        Arc::new(Self {
            host,
            settings,
            env,
            state: Mutex::new(State::default()),
        })
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Mounts the panel: a column in fullscreen layout, otherwise a block above the editor with
    /// the one-time notice. Background loads are started by the caller ([`Panel::initial_load`]).
    pub fn start(self: &Arc<Self>) {
        let mode = if self.env.fullscreen {
            MountMode::Column
        } else {
            MountMode::Block
        };
        {
            let mut st = self.lock();
            st.generation += 1;
            st.mounted = Some(mode);
            st.mounted_at_ms = self.host.now_ms();
        }
        self.host.mount(mode);
        if mode == MountMode::Block && layout::claim_notice(&self.env.agent_dir) {
            self.host
                .notify(&layout::fullscreen_notice(), NotificationType::Info);
        }
    }

    /// The host switched layouts; in fullscreen the block is replaced by the column.
    pub fn set_fullscreen(&self, fullscreen: bool) {
        let target = if fullscreen {
            MountMode::Column
        } else {
            MountMode::Block
        };
        let changed = {
            let mut st = self.lock();
            match st.mounted {
                Some(current) if current != target => {
                    st.mounted = Some(target);
                    true
                }
                _ => false,
            }
        };
        if changed {
            self.host.unmount();
            self.host.mount(target);
        }
    }

    /// Re-attaches after the host rebuilt its layout and dropped the column.
    pub fn reattach(&self) {
        let mode = self.lock().mounted;
        if let Some(mode) = mode {
            self.host.mount(mode);
        }
    }

    pub fn mount_mode(&self) -> Option<MountMode> {
        self.lock().mounted
    }

    pub fn is_mounted(&self) -> bool {
        self.lock().mounted.is_some()
    }

    /// Effective column width at terminal width `terminal`.
    pub fn width(&self, terminal: usize) -> usize {
        layout::effective_width(self.settings.width, terminal)
    }

    /// Whether the column is shown at terminal width `terminal` (hidden, not unmounted, below
    /// `min_columns`).
    pub fn column_visible(&self, terminal: usize) -> bool {
        layout::column_visible(self.settings.min_columns, terminal)
    }

    fn clickable(&self) -> bool {
        self.settings.clickable && self.env.links_supported
    }

    /// The current view model.
    pub fn view(&self) -> PanelView {
        let st = self.lock();
        let now = self.host.now_ms();
        let mut facts = st.facts.clone();
        facts.elapsed_ms = now.saturating_sub(st.mounted_at_ms);
        let location = sections::location(
            &self.env.cwd,
            self.env.home.as_deref(),
            st.branch.as_deref(),
        );
        PanelView {
            sections: self.settings.sections,
            clickable: self.clickable(),
            session: facts,
            goal: st.goal.clone(),
            usage: st.usage.clone(),
            tasks: st.board.clone(),
            now_ms: now,
            tools: st.tools.clone(),
            files: st.files.clone(),
            memory: st.memory.clone(),
            location,
        }
    }

    /// Renders the rows at `width` cells.
    pub fn render(&self, width: usize, painter: sections::Painter<'_>) -> Vec<String> {
        if !self.is_mounted() {
            return Vec::new();
        }
        sections::render(&self.view(), width, painter)
    }

    // ---- host facts -------------------------------------------------------------------------

    /// Turn end / agent settle / message end: update facts, re-read the goal file and repaint.
    pub fn update_facts(&self, facts: SessionFacts) {
        {
            let mut st = self.lock();
            if st.mounted.is_none() {
                return;
            }
            st.facts = facts;
            self.reload_goal(&mut st);
        }
        self.host.repaint();
    }

    fn reload_goal(&self, st: &mut State) {
        st.goal = self
            .env
            .goal_file
            .as_deref()
            .and_then(|path| st.goal_reader.read(path));
    }

    /// Re-reads the goal file (only parsed when mtime or size changed).
    pub fn refresh_goal(&self) {
        let mut st = self.lock();
        self.reload_goal(&mut st);
    }

    /// Number of goal file parses so far.
    pub fn goal_parses(&self) -> u64 {
        self.lock().goal_reader.parses
    }

    // ---- tools ------------------------------------------------------------------------------

    pub fn record_tool(&self, name: &str, args: &Value) {
        let mut st = self.lock();
        if st.mounted.is_some() {
            st.tools.record(name, args);
        }
    }

    /// User input clears the tool list.
    pub fn user_input(&self) {
        self.lock().tools.clear();
        self.host.repaint();
    }

    // ---- tasks ------------------------------------------------------------------------------

    /// Re-reads the task state directory. An unreadable directory is logged at debug level and
    /// leaves the list unchanged.
    pub fn refresh_tasks(self: &Arc<Self>) {
        let Some(dir) = self.env.task_dir.clone() else {
            return;
        };
        if !self.is_mounted() {
            return;
        }
        match tasks::read_tasks(&dir, self.env.session_id.as_deref()) {
            Ok(updates) => {
                for update in updates {
                    self.apply_task(update);
                }
            }
            Err(error) => self.host.debug(&format!(
                "side panel: task directory {}: {error}",
                dir.display()
            )),
        }
    }

    /// Applies one task update, then starts the duration ticker when a task is active.
    pub fn apply_task(self: &Arc<Self>, update: TaskUpdate) {
        if let Some(owner) = &update.owner_session
            && self.env.session_id.as_deref() != Some(owner.as_str())
        {
            return;
        }
        if self.env.session_id.is_none() {
            return;
        }
        {
            let mut st = self.lock();
            if st.mounted.is_none() {
                return;
            }
            st.board.apply(update);
            if st.ticker_running && !st.board.any_active() {
                // Stop the ticker as soon as no task is active.
                st.ticker_running = false;
                st.ticker_id += 1;
            }
        }
        self.ensure_ticker();
    }

    pub fn board(&self) -> TaskBoard {
        self.lock().board.clone()
    }

    pub fn ticker_running(&self) -> bool {
        self.lock().ticker_running
    }

    fn ensure_ticker(self: &Arc<Self>) {
        let (generation, ticker) = {
            let mut st = self.lock();
            if st.ticker_running || st.mounted.is_none() || !st.board.any_active() {
                return;
            }
            let Ok(_) = tokio::runtime::Handle::try_current() else {
                return;
            };
            st.ticker_running = true;
            st.ticker_id += 1;
            (st.generation, st.ticker_id)
        };
        let panel = Arc::clone(self);
        tokio::spawn(async move {
            let start = tokio::time::Instant::now() + TICK;
            let mut interval = tokio::time::interval_at(start, TICK);
            loop {
                interval.tick().await;
                let keep = {
                    let mut st = panel.lock();
                    if st.generation != generation || st.ticker_id != ticker {
                        false
                    } else if !st.board.any_active() {
                        st.ticker_running = false;
                        false
                    } else {
                        true
                    }
                };
                if !keep {
                    break;
                }
                panel.host.repaint();
            }
        });
    }

    // ---- git --------------------------------------------------------------------------------

    /// Number of `git status` runs so far.
    pub fn git_runs(&self) -> u64 {
        self.lock().git_runs
    }

    pub fn files(&self) -> Option<Vec<ChangedFile>> {
        self.lock().files.clone()
    }

    /// Refreshes the files section: at most once per 2 s unless `forced`. Failures keep the
    /// previous data; results that land after shutdown (or for another session) are dropped.
    pub async fn refresh_git(self: &Arc<Self>, forced: bool) {
        let generation = {
            let mut st = self.lock();
            if st.mounted.is_none() {
                return;
            }
            let now = self.host.now_ms();
            if !st.git_throttle.admit(now, git::THROTTLE, forced) {
                return;
            }
            st.git_runs += 1;
            st.generation
        };
        let cwd = self.env.cwd.clone();
        let branch = git::branch(&cwd);
        let root = self
            .host
            .git(
                vec!["rev-parse".into(), "--show-toplevel".into()],
                cwd.clone(),
                git::TIMEOUT_MS,
            )
            .await;
        let outcome = match root {
            Ok(result) if result.code != 0 => Some((None, None)),
            Ok(result) => {
                let root = PathBuf::from(result.stdout.trim());
                let status = self
                    .host
                    .git(git::status_args(), root.clone(), git::TIMEOUT_MS)
                    .await;
                let staged = self
                    .host
                    .git(git::numstat_args(true), root.clone(), git::TIMEOUT_MS)
                    .await;
                let unstaged = self
                    .host
                    .git(git::numstat_args(false), root.clone(), git::TIMEOUT_MS)
                    .await;
                match (status, staged, unstaged) {
                    (Ok(s), Ok(a), Ok(b)) if s.code == 0 => Some((
                        Some(git::merge(&s.stdout, &a.stdout, &b.stdout)),
                        Some(root),
                    )),
                    _ => None,
                }
            }
            Err(_) => None,
        };
        {
            let mut st = self.lock();
            if st.generation != generation || st.mounted.is_none() {
                return;
            }
            st.branch = branch;
            if let Some((files, root)) = outcome {
                st.files = files;
                st.repo_root = root;
            }
        }
        self.host.repaint();
    }

    /// Refreshes memory status at most once per 5 s unless `forced`.
    pub fn refresh_memory(&self, forced: bool) {
        let Some(reader) = self.env.memory.clone() else {
            return;
        };
        {
            let mut st = self.lock();
            if st.mounted.is_none() {
                return;
            }
            let now = self.host.now_ms();
            if !st.memory_throttle.admit(now, MEMORY_THROTTLE, forced) {
                return;
            }
        }
        let info = reader();
        self.lock().memory = info;
        self.host.repaint();
    }

    /// Polls plan usage (rows hidden on failure).
    pub async fn refresh_usage(&self) {
        if !self.settings.sections.usage {
            return;
        }
        let Some(reader) = self.env.usage.clone() else {
            return;
        };
        let generation = self.lock().generation;
        let windows = reader().await.unwrap_or_default();
        let mut st = self.lock();
        if st.generation == generation && st.mounted.is_some() {
            st.usage = windows;
        }
    }

    /// Starts the usage poller (every `usage_poll_seconds`) when the usage section is on.
    pub fn start_usage_poller(self: &Arc<Self>) {
        if !self.settings.sections.usage || self.env.usage.is_none() {
            return;
        }
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        let generation = self.lock().generation;
        let panel = Arc::clone(self);
        let period = Duration::from_secs(u64::from(self.settings.usage_poll_seconds));
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(period);
            loop {
                interval.tick().await;
                if panel.lock().generation != generation {
                    break;
                }
                panel.refresh_usage().await;
                panel.host.repaint();
            }
        });
    }

    /// Session start loads: tasks, git (forced) and memory (forced).
    pub async fn initial_load(self: &Arc<Self>) {
        self.refresh_tasks();
        self.refresh_memory(true);
        self.refresh_git(true).await;
    }

    /// Turn end / agent settle / message end: refresh tasks, git and memory.
    pub async fn after_turn(self: &Arc<Self>, facts: SessionFacts) {
        self.update_facts(facts);
        self.refresh_tasks();
        self.refresh_memory(false);
        self.refresh_git(false).await;
        self.refresh_usage().await;
    }

    /// Tool start: record the call, then refresh.
    pub async fn tool_started(self: &Arc<Self>, name: &str, args: &Value) {
        self.record_tool(name, args);
        self.host.repaint();
        self.refresh_tasks();
        self.refresh_git(false).await;
    }

    /// Tool end: refresh git only.
    pub async fn tool_ended(self: &Arc<Self>) {
        self.refresh_git(false).await;
    }

    /// Session shutdown: stop timers and pollers, unmount and clear all state.
    pub fn shutdown(&self) {
        let was_mounted = {
            let mut st = self.lock();
            let generation = st.generation + 1;
            let was = st.mounted.is_some();
            *st = State {
                generation,
                ..State::default()
            };
            was
        };
        if was_mounted {
            self.host.unmount();
        }
    }

    // ---- actions ----------------------------------------------------------------------------

    /// Claims well-formed `mhc-panel:` URLs and opens them; other URLs return `false` and go to
    /// the previous handler.
    pub fn handle_link(self: &Arc<Self>, url: &str) -> bool {
        let Some(link) = Link::parse(url) else {
            return false;
        };
        if tokio::runtime::Handle::try_current().is_ok() {
            let panel = Arc::clone(self);
            tokio::spawn(async move { panel.open(link).await });
        }
        true
    }

    /// Opens the detail view of `link`.
    pub async fn open(self: &Arc<Self>, link: Link) {
        match link {
            Link::File(path) => self.open_file(&path).await,
            Link::Agent(id) => self.open_agent(&id),
            Link::Goal => self.open_goal(),
            Link::Memory => self.open_memory(),
        }
    }

    fn terminal_rows(&self) -> usize {
        DEFAULT_TERMINAL_ROWS
    }

    /// Opens the diff viewer of a changed file; a file no longer listed gives an info notice.
    pub async fn open_file(self: &Arc<Self>, path: &str) {
        let (file, root) = {
            let st = self.lock();
            let file = st
                .files
                .as_ref()
                .and_then(|files| files.iter().find(|f| f.path == path).cloned());
            (file, st.repo_root.clone())
        };
        let Some(file) = file else {
            self.host.notify(
                &format!("{path} has no changes any more."),
                NotificationType::Info,
            );
            return;
        };
        let root = root.unwrap_or_else(|| self.env.cwd.clone());
        let rows = self.diff_rows(&file, root).await;
        let rows = rows
            .into_iter()
            .map(|row| match row {
                DiffRow::Error(_) => ViewerRow::error(row.text()),
                other => ViewerRow::plain(other.text()),
            })
            .collect();
        self.host
            .open_viewer(Viewer::new(file.path.clone(), rows, self.terminal_rows()));
    }

    async fn diff_rows(&self, file: &ChangedFile, root: PathBuf) -> Vec<DiffRow> {
        if file.untracked() && file.path.ends_with('/') {
            return match self
                .host
                .git(git::untracked_dir_args(&file.path), root, git::TIMEOUT_MS)
                .await
            {
                Ok(result) if result.code == 0 => git::untracked_dir_rows(&result.stdout),
                Ok(result) => vec![DiffRow::Error(result.code)],
                Err(_) => vec![DiffRow::Error(-1)],
            };
        }
        match self
            .host
            .git(git::diff_args(file), root, git::TIMEOUT_MS)
            .await
        {
            Ok(result) => git::diff_rows(&result.stdout, result.code, file.untracked()),
            Err(_) => vec![DiffRow::Error(-1)],
        }
    }

    /// Opens the task viewer: a summary card and the recorded output.
    pub fn open_agent(&self, id: &str) {
        let task = self.lock().board.get(id).cloned();
        let Some(task) = task else {
            self.host
                .notify(&format!("Task {id} is not listed."), NotificationType::Info);
            return;
        };
        let record = &task.record;
        let field = |key: &str| {
            record
                .get(key)
                .map(|v| match v {
                    Value::String(s) => s.clone(),
                    Value::Null => "-".into(),
                    other => other.to_string(),
                })
                .unwrap_or_else(|| "-".into())
        };
        let mut rows = vec![
            ViewerRow::plain(format!("id        {}", task.id)),
            ViewerRow::plain(format!("status    {}", field("status"))),
            ViewerRow::plain(format!("host      {}", field("model"))),
            ViewerRow::plain(format!("category  {}", field("category"))),
            ViewerRow::plain(format!("turns     {}", field("runs"))),
            ViewerRow::plain(format!("tokens    {}", field("tokens"))),
            ViewerRow::plain(format!("cost      ${:.2}", task.cost)),
            ViewerRow::plain(String::new()),
        ];
        let output = self
            .env
            .task_dir
            .as_ref()
            .and_then(|dir| std::fs::read_to_string(dir.join(format!("{id}.md"))).ok());
        match output {
            Some(text) if !text.trim().is_empty() => {
                for line in text.lines() {
                    let lower = line.trim_start().to_ascii_lowercase();
                    if lower.starts_with("error") || lower.starts_with("fatal") {
                        rows.push(ViewerRow::error(line));
                    } else {
                        rows.push(ViewerRow::plain(line));
                    }
                }
            }
            _ => rows.push(ViewerRow::plain("(no output recorded yet)")),
        }
        if let Some(reason) = &task.reason {
            rows.push(ViewerRow::error(reason.clone()));
        }
        self.host
            .open_viewer(Viewer::new(task.name.clone(), rows, self.terminal_rows()));
    }

    pub fn open_goal(&self) {
        let goal = self.lock().goal.clone();
        if let Some(goal) = goal {
            self.host.open_viewer(Viewer::new(
                format!("Goal ({})", goal.status),
                viewer::goal_rows(&goal.objective),
                self.terminal_rows(),
            ));
        }
    }

    pub fn open_memory(&self) {
        let memory = self.lock().memory.clone();
        if let Some(memory) = memory {
            let rows = memory.rows.iter().map(ViewerRow::plain).collect();
            self.host.open_viewer(Viewer::new(
                format!("Memory {}", memory.identity),
                rows,
                self.terminal_rows(),
            ));
        }
    }

    /// `/side-panel-diff`: pick a changed file, then open its diff.
    pub async fn diff_command(self: &Arc<Self>) {
        if !self.is_mounted() {
            self.host.notify(
                "The side panel is not shown in this session.",
                NotificationType::Info,
            );
            return;
        }
        let probe = self
            .host
            .git(
                vec!["--version".into()],
                self.env.cwd.clone(),
                git::TIMEOUT_MS,
            )
            .await;
        if probe.is_err() {
            self.host.notify(
                "Cannot list changes: this session cannot run git commands.",
                NotificationType::Warning,
            );
            return;
        }
        self.refresh_git(true).await;
        let files = self.files().unwrap_or_default();
        if files.is_empty() {
            self.host
                .notify("No changed files.", NotificationType::Info);
            return;
        }
        let labels: Vec<String> = files
            .iter()
            .map(|f| crate::text::sanitize(&f.label()))
            .collect();
        let Some(choice) = self.host.pick("Changed files".into(), labels.clone()).await else {
            return;
        };
        if let Some(index) = labels.iter().position(|l| *l == choice) {
            self.open_file(&files[index].path).await;
        }
    }
}

/// Maps a style to a theme painter for real rendering.
pub fn theme_painter(theme: &maho_ext_api::Theme) -> impl Fn(Style, &str) -> String + '_ {
    move |style, text| theme.fg(sections::theme_color(style), text)
}
