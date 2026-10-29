use crate::{
    commands::{
        PanelCommandContext, PanelCommandDeps, PanelExec, open_file_diff, run_diff_command,
    },
    constants::{
        GIT_REFRESH_FLOOR_MS, GOAL_OBJECTIVE_COLUMNS, LIVE_REFRESH_MS, MEMORY_REFRESH_FLOOR_MS,
    },
    context::{PanelEventContext, panel_context_from},
    format::{sanitize_terminal_text, wrap_visible},
    host_surface::{Defer, PanelHostSurface, PanelHostSurfaceDeps, create_panel_host_surface},
    links::PanelAction,
    rows::{PanelRowsInput, build_panel_rows},
    settings::OmoSidePanelSettings,
    store::{PanelChild, PanelState, PanelToolCall},
    types::*,
    width::resolve_panel_width,
};
use serde_json::Value;
use std::sync::{Arc, Mutex};
#[derive(Clone, Debug, Default)]
pub struct PanelHostFacts {
    pub model: Option<String>,
    pub session_id: Option<String>,
    pub goal_store_file: Option<String>,
    pub value: Value,
}
pub trait PanelReaders: Send + Sync {
    fn facts(&self, event: &Value, previous: &PanelHostFacts) -> PanelHostFacts {
        panel_facts_from(event, previous)
    }
    fn find_git_root(&self, cwd: &str) -> Option<String>;
    fn git_branch(&self, root: &str) -> Option<String>;
    fn git<'a>(&'a self, root: &'a str) -> PanelFuture<'a, Option<PanelGitStatus>>;
    fn children<'a>(
        &'a self,
        cwd: &'a str,
        session_id: &'a str,
    ) -> PanelFuture<'a, Result<Vec<PanelChild>, String>>;
    fn goal(&self, path: Option<&str>) -> Option<PanelGoal>;
    fn resolve_memory(&self, cwd: &str) -> Option<String>;
    fn memory<'a>(
        &'a self,
        identity: &'a str,
        session_id: Option<&'a str>,
    ) -> PanelFuture<'a, Option<PanelMemory>>;
    fn child_output<'a>(&'a self, cwd: &'a str, id: &'a str) -> PanelFuture<'a, Vec<PanelRow>>;
    fn sections(&self, input: &PanelSnapshot, width: usize) -> PanelRowsInput {
        crate::sections::build_snapshot_sections(
            input,
            width,
            std::env::var("HOME").ok().as_deref(),
        )
    }
}
pub trait UsageSource: Send + Sync {
    fn start(&self);
    fn refresh(&self);
    fn stop(&self);
    fn rows(&self, now: u64, width: usize) -> Vec<PanelRow>;
}
pub type UsageFactory =
    Arc<dyn Fn(Arc<dyn Fn() + Send + Sync>) -> Arc<dyn UsageSource> + Send + Sync>;
pub trait PanelTimers: Send + Sync {
    fn set(&self, ms: u64, callback: Box<dyn FnOnce() + Send>) -> Stop;
}
pub struct TokioPanelTimers;
impl PanelTimers for TokioPanelTimers {
    fn set(&self, ms: u64, callback: Box<dyn FnOnce() + Send>) -> Stop {
        let task = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
            callback();
        });
        Box::new(move || task.abort())
    }
}
#[derive(Clone)]
pub struct SidePanelRuntimeOptions {
    pub cwd: String,
    pub regular_notice_state_dir: Option<std::path::PathBuf>,
    pub now: Arc<dyn Fn() -> u64 + Send + Sync>,
    pub readers: Arc<dyn PanelReaders>,
    pub exec: Option<Arc<dyn PanelExec>>,
    pub timers: Arc<dyn PanelTimers>,
    pub defer: Defer,
    pub usage: Option<UsageFactory>,
    pub track: Option<Arc<dyn Fn(tokio::task::JoinHandle<()>) + Send + Sync>>,
    pub debug: Arc<dyn Fn(&str) + Send + Sync>,
}
#[derive(Clone)]
pub struct PanelSnapshot {
    pub settings: OmoSidePanelSettings,
    pub facts: PanelHostFacts,
    pub state: PanelState,
    pub cwd: String,
    pub branch: Option<String>,
    pub started_at: u64,
    pub now: u64,
    pub git: Option<PanelGitStatus>,
    pub goal: Option<PanelGoal>,
    pub memory: Option<PanelMemory>,
}
#[derive(Default)]
struct ControllerState {
    epoch: u64,
    surface: Option<Arc<PanelHostSurface>>,
    usage: Option<Arc<dyn UsageSource>>,
    ui: Option<Arc<dyn PanelUi>>,
    settings: Option<OmoSidePanelSettings>,
    facts: PanelHostFacts,
    store: PanelState,
    git: Option<PanelGitStatus>,
    git_root: Option<String>,
    branch: Option<String>,
    git_read_at: u64,
    memory: Option<PanelMemory>,
    memory_identity: Option<String>,
    memory_read_at: u64,
    goal: Option<PanelGoal>,
    started_at: u64,
    live_timer: Option<Stop>,
}
pub struct SidePanelController {
    options: SidePanelRuntimeOptions,
    state: Mutex<ControllerState>,
}
pub fn create_side_panel_controller(options: SidePanelRuntimeOptions) -> Arc<SidePanelController> {
    Arc::new(SidePanelController {
        options,
        state: Mutex::default(),
    })
}
impl SidePanelController {
    fn snapshot(&self) -> Option<PanelSnapshot> {
        let s = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Some(PanelSnapshot {
            settings: s.settings.clone()?,
            facts: s.facts.clone(),
            state: s.store.clone(),
            cwd: self.options.cwd.clone(),
            branch: s.branch.clone(),
            started_at: s.started_at,
            now: (self.options.now)(),
            git: s.git.clone(),
            goal: s.goal.clone(),
            memory: s.memory.clone(),
        })
    }
    pub fn rows(&self, width: usize) -> Vec<PanelRow> {
        let Some(snapshot) = self.snapshot() else {
            return Vec::new();
        };
        let mut input = self.options.readers.sections(&snapshot, width);
        input.sections = snapshot.settings.sections.clone();
        let usage = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .usage
            .clone();
        if let Some(usage) = usage {
            input.usage = usage.rows(snapshot.now, width);
        }
        build_panel_rows(&input, width)
    }
    fn repaint(&self) {
        let surface = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .surface
            .clone();
        if let Some(surface) = surface {
            surface.request_render();
        }
    }
    fn background(self: &Arc<Self>, work: PanelFuture<'static, ()>) {
        let task = tokio::spawn(work);
        if let Some(track) = &self.options.track {
            track(task);
        }
    }
    fn schedule_live_refresh(self: &Arc<Self>) {
        let mut s = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if s.live_timer.is_some()
            || s.surface.is_none()
            || !s.store.children.iter().any(|c| c.status.running())
        {
            return;
        }
        let epoch = s.epoch;
        let weak = Arc::downgrade(self);
        s.live_timer = Some(self.options.timers.set(
            LIVE_REFRESH_MS,
            Box::new(move || {
                if let Some(controller) = weak.upgrade() {
                    let mut s = controller
                        .state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if s.epoch != epoch {
                        return;
                    }
                    s.live_timer = None;
                    drop(s);
                    controller.repaint();
                    controller.schedule_live_refresh();
                }
            }),
        ));
    }
    async fn refresh_git(&self, force: bool) {
        let (root, epoch) = {
            let mut s = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(root) = s.git_root.clone().filter(|_| self.options.exec.is_some()) else {
                return;
            };
            let at = (self.options.now)();
            if !force && at.saturating_sub(s.git_read_at) < GIT_REFRESH_FLOOR_MS {
                return;
            }
            s.git_read_at = at;
            s.branch = self.options.readers.git_branch(&root);
            (root, s.epoch)
        };
        let next = self.options.readers.git(&root).await;
        let mut s = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if s.epoch != epoch {
            return;
        }
        if let Some(next) = next {
            s.git = Some(next);
            drop(s);
            self.repaint();
        }
    }
    async fn refresh_memory(&self, force: bool) {
        let (identity, session, epoch) = {
            let mut s = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(identity) = s.memory_identity.clone() else {
                return;
            };
            let at = (self.options.now)();
            if !force && at.saturating_sub(s.memory_read_at) < MEMORY_REFRESH_FLOOR_MS {
                return;
            }
            s.memory_read_at = at;
            (identity, s.facts.session_id.clone(), s.epoch)
        };
        let next = self
            .options
            .readers
            .memory(&identity, session.as_deref())
            .await;
        let mut s = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if s.epoch != epoch {
            return;
        }
        if let Some(next) = next {
            s.memory = Some(next);
            drop(s);
            self.repaint();
        }
    }
    async fn refresh_children(self: &Arc<Self>) {
        let (session, epoch) = {
            let s = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(settings) = &s.settings else {
                return;
            };
            if !(settings.sections.agents || settings.sections.session) {
                return;
            }
            let Some(session) = s.facts.session_id.clone() else {
                return;
            };
            (session, s.epoch)
        };
        let records = match self
            .options
            .readers
            .children(&self.options.cwd, &session)
            .await
        {
            Ok(records) => records,
            Err(error) => {
                (self.options.debug)(&format!(
                    "omo-senpi side panel: task records unreadable: {error}"
                ));
                return;
            }
        };
        let changed = {
            let mut s = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if s.epoch != epoch {
                return;
            }
            let mut changed = false;
            for child in records {
                changed |= s.store.merge_child(child.into(), (self.options.now)());
            }
            changed
        };
        if changed {
            self.repaint();
            self.schedule_live_refresh();
        }
    }
    pub async fn open_action(&self, action: PanelAction) {
        let (ui, git, goal, memory, children, epoch) = {
            let s = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            (
                s.ui.clone(),
                s.git.clone(),
                s.goal.clone(),
                s.memory.clone(),
                s.store.children.clone(),
                s.epoch,
            )
        };
        let Some(ui) = ui else {
            return;
        };
        match action {
            PanelAction::File { path } => {
                let (Some(git), Some(exec)) = (git, &self.options.exec) else {
                    return;
                };
                let Some(file) = git.files.iter().find(|f| f.path == path) else {
                    ui.notify(
                        &sanitize_terminal_text(&format!("{path} is no longer listed as changed.")),
                        "info",
                    );
                    return;
                };
                open_file_diff(ui.as_ref(), exec.as_ref(), &git, file).await;
            }
            PanelAction::Goal => {
                if let Some(goal) = goal {
                    ui.viewer(
                        "goal",
                        wrap_visible(&goal.objective, GOAL_OBJECTIVE_COLUMNS)
                            .into_iter()
                            .map(PanelRow::new)
                            .collect(),
                    )
                    .await;
                }
            }
            PanelAction::Memory => {
                if let Some(memory) = memory {
                    ui.viewer("memory", memory.detail).await;
                }
            }
            PanelAction::Agent { id } => {
                if let Some(child) = children.iter().find(|c| c.id == id) {
                    let output = self
                        .options
                        .readers
                        .child_output(&self.options.cwd, &id)
                        .await;
                    if self
                        .state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .epoch
                        != epoch
                    {
                        return;
                    }
                    let mut rows = child.card.clone();
                    rows.push(PanelRow::default());
                    rows.extend(output);
                    ui.viewer(&format!("{}  (agent)", child.name), rows).await;
                }
            }
        }
    }
    async fn refresh_inner(self: &Arc<Self>, event: Value, epoch: u64) {
        {
            let mut s = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if s.surface.is_none() || s.epoch != epoch {
                return;
            }
            s.facts = self.options.readers.facts(&event, &s.facts);
            if s.settings.as_ref().is_some_and(|a| a.sections.goal) {
                s.goal = self
                    .options
                    .readers
                    .goal(s.facts.goal_store_file.as_deref());
            }
        }
        self.repaint();
        let usage = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .usage
            .clone();
        if let Some(usage) = usage {
            usage.refresh();
        }
        futures::join!(
            self.refresh_children(),
            self.refresh_git(false),
            self.refresh_memory(false)
        );
        self.schedule_live_refresh();
    }
    pub fn refresh(self: &Arc<Self>, event: Value) {
        let epoch = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .epoch;
        let this = self.clone();
        self.background(Box::pin(async move {
            this.refresh_inner(event, epoch).await;
        }));
    }
    pub fn tool_start(self: &Arc<Self>, payload: &Value, event: Value) {
        let mut s = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if s.surface.is_none() {
            return;
        }
        if let Some(call) = tool_call_from(payload, (self.options.now)()) {
            s.store.record_tool(call);
        }
        drop(s);
        self.refresh(event);
    }
    pub fn tool_end(self: &Arc<Self>) {
        let epoch = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .epoch;
        let this = self.clone();
        self.background(Box::pin(async move {
            if this
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .epoch
                == epoch
            {
                this.refresh_git(false).await;
            }
        }));
    }
    pub fn input(&self) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .store
            .clear_tools();
    }
    pub fn teardown(&self) {
        let (timer, usage, surface) = {
            let mut s = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let epoch = s.epoch.wrapping_add(1);
            let timer = s.live_timer.take();
            let usage = s.usage.take();
            let surface = s.surface.take();
            *s = ControllerState {
                epoch,
                ..ControllerState::default()
            };
            (timer, usage, surface)
        };
        if let Some(stop) = timer {
            stop();
        }
        if let Some(usage) = usage {
            usage.stop();
        }
        if let Some(surface) = surface {
            surface.dispose();
        }
    }
    pub async fn session_start(
        self: &Arc<Self>,
        settings: OmoSidePanelSettings,
        event: &PanelEventContext,
    ) {
        if self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .surface
            .is_some()
        {
            return;
        }
        let Some(context) = panel_context_from(event) else {
            (self.options.debug)("omo-senpi side panel: host context carries no ui, staying dark");
            return;
        };
        let facts = self
            .options
            .readers
            .facts(&event.value, &PanelHostFacts::default());
        let goal = if settings.sections.goal {
            self.options.readers.goal(facts.goal_store_file.as_deref())
        } else {
            None
        };
        let git_root = if settings.sections.files {
            self.options.readers.find_git_root(&self.options.cwd)
        } else {
            None
        };
        let memory_identity = if settings.sections.memory {
            self.options.readers.resolve_memory(&self.options.cwd)
        } else {
            None
        };
        let weak = Arc::downgrade(self);
        let action_weak = weak.clone();
        let configured = settings.width.clone();
        let notice_dir = self
            .options
            .regular_notice_state_dir
            .clone()
            .unwrap_or_else(default_regular_notice_state_dir);
        let surface = create_panel_host_surface(PanelHostSurfaceDeps {
            context: context.clone(),
            source: Arc::new(move |width| weak.upgrade().map_or_else(Vec::new, |c| c.rows(width))),
            width: Arc::new(move |terminal| resolve_panel_width(&configured, terminal)),
            min_columns: settings.min_columns,
            clickable: settings.clickable,
            on_action: Some(Arc::new(move |action| {
                if let Some(c) = action_weak.upgrade() {
                    let this = c.clone();
                    c.background(Box::pin(async move {
                        this.open_action(action).await;
                    }));
                }
            })),
            defer: self.options.defer.clone(),
            on_regular_mode_fallback: Some({
                let ui = context.ui.clone();
                Arc::new(move || {
                    if claim_regular_mode_notice(&notice_dir) {
                        ui.notify(REGULAR_MODE_NOTICE_MESSAGE, "info");
                    }
                })
            }),
        });
        let usage = if settings.sections.usage {
            self.options.usage.as_ref().map(|factory| {
                let weak = Arc::downgrade(self);
                factory(Arc::new(move || {
                    if let Some(c) = weak.upgrade() {
                        c.repaint();
                    }
                }))
            })
        } else {
            None
        };
        {
            let mut s = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            s.facts = facts;
            s.goal = goal;
            s.git_root = git_root;
            s.memory_identity = memory_identity;
            s.settings = Some(settings);
            s.started_at = (self.options.now)();
            s.ui = Some(context.ui);
            s.surface = Some(surface.clone());
            s.usage = usage.clone();
        }
        if let Some(usage) = usage {
            usage.start();
        }
        surface.mount();
        let epoch = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .epoch;
        let controller = self.clone();
        self.background(Box::pin(async move {
            if controller
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .epoch
                != epoch
            {
                return;
            }
            futures::join!(
                controller.refresh_children(),
                controller.refresh_git(true),
                controller.refresh_memory(true)
            );
            controller.schedule_live_refresh();
        }));
    }
    pub async fn run_diff_command(&self, ctx: &PanelCommandContext) {
        let (git, mounted) = {
            let s = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            (s.git.clone(), s.surface.is_some())
        };
        run_diff_command(
            PanelCommandDeps {
                status: git.as_ref(),
                exec: self.options.exec.as_deref(),
                mounted,
            },
            ctx,
        )
        .await;
    }
}
pub fn tool_call_from(payload: &Value, at: u64) -> Option<PanelToolCall> {
    let record = payload.as_object()?;
    let name = record
        .get("toolName")
        .filter(|v| !v.is_null())
        .or_else(|| record.get("name"))?
        .as_str()
        .filter(|s| !s.is_empty())?;
    let args = record
        .get("args")
        .filter(|v| !v.is_null())
        .or_else(|| record.get("input"))
        .and_then(Value::as_object);
    let detail = args
        .and_then(|args| {
            ["file_path", "path", "command", "pattern", "query", "url"]
                .into_iter()
                .find_map(|key| {
                    args.get(key)
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                })
        })
        .map(str::to_owned);
    Some(PanelToolCall {
        name: name.into(),
        detail,
        at,
    })
}
pub use crate::data::facts::panel_facts_from;
#[cfg(test)]
mod tests;

pub const REGULAR_MODE_NOTICE_MARKER: &str = "side-panel-regular-mode-notice";
pub const REGULAR_MODE_NOTICE_MESSAGE: &str = "side_panel: the column beside the transcript needs the fullscreen TUI. In the default regular mode the panel renders as a block above the editor. Use --tui-mode fullscreen, or set tuiMode to \"fullscreen\" in senpi's settings, for the side-by-side column.";

pub fn claim_regular_mode_notice(state_dir: &std::path::Path) -> bool {
    use std::io::Write;
    let claim = || -> std::io::Result<()> {
        std::fs::create_dir_all(state_dir)?;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(state_dir.join(REGULAR_MODE_NOTICE_MARKER))?;
        write!(
            file,
            "{}",
            serde_json::json!({"shownAt": chrono::Utc::now().to_rfc3339()})
        )
    };
    // The notice is optional; inability to claim deliberately fails closed.
    claim().is_ok()
}
fn default_regular_notice_state_dir() -> std::path::PathBuf {
    let env = std::env::vars().collect();
    maho_omo_agent_home::resolve_agent_home(
        &env,
        &std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default()),
        &std::env::current_dir().unwrap_or_default(),
        std::path::Path::exists,
    )
    .join("omo-senpi/omo-native")
}
