use std::{path::PathBuf, sync::Arc};
use maho_ext_api::{ExtensionContext, ExtensionMode, ExtensionUi, ModelRegistry};
use senpi_task::completion::{ParentState, TransitionReason};

pub struct TaskRuntimeContext {
    cwd: PathBuf,
    model_registry: Option<Arc<dyn ModelRegistry>>,
    idle: bool,
    transition: Option<TransitionReason>,
    ui: Option<Arc<dyn ExtensionUi>>,
    session_id: Option<String>,
    session_file: Option<PathBuf>,
    mode: Option<ExtensionMode>,
}
impl TaskRuntimeContext {
    pub fn new(cwd: PathBuf) -> Self { Self { cwd, model_registry: None, idle: true, transition: None, ui: None, session_id: None, session_file: None, mode: None } }
    pub fn capture_from(&mut self, ctx: &ExtensionContext) {
        if !ctx.cwd.as_os_str().is_empty() { self.cwd = ctx.cwd.clone(); }
        self.model_registry = Some(ctx.model_registry.clone());
        self.ui = Some(ctx.ui.clone());
        self.mode = Some(ctx.mode);
        self.session_id = Some(ctx.session_manager.session_id().into());
        self.session_file = ctx.session_manager.session_file().map(ToOwned::to_owned);
        self.idle = (ctx.is_idle_fn)();
    }
    pub fn clear_ui(&mut self) { self.ui = None; }
    pub fn set_transition(&mut self, transition: Option<TransitionReason>) { self.transition = transition; }
    pub fn cwd(&self) -> &std::path::Path { &self.cwd }
    pub fn model_registry(&self) -> Option<&Arc<dyn ModelRegistry>> { self.model_registry.as_ref() }
    pub fn ui(&self) -> Option<&Arc<dyn ExtensionUi>> { self.ui.as_ref() }
    pub fn session_id(&self) -> Option<&str> { self.session_id.as_deref() }
    pub fn session_file(&self) -> Option<&std::path::Path> { self.session_file.as_deref() }
    pub fn mode(&self) -> Option<ExtensionMode> { self.mode }
    pub fn parent_state(&self) -> ParentState {
        match self.transition {
            Some(TransitionReason::Compacting) => ParentState::Compacting,
            Some(TransitionReason::SessionSwitching) => ParentState::SessionSwitching,
            Some(TransitionReason::SessionShutdown) => ParentState::SessionShutdown,
            None => if self.idle { ParentState::Idle } else { ParentState::Streaming },
        }
    }
}
