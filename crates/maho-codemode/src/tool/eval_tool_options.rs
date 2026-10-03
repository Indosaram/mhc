use std::{collections::HashMap,path::PathBuf,sync::{Arc,Mutex}};
use maho_ai::utils::abort::AbortSignal;
use maho_ext_api::AgentToolResult;
use crate::{bridges::{output_bridge::OutputExecuteTool,schema_bridge::EvalToolCatalog},config::settings::CodemodeSettings,prompt::eval_prompt::EvalPromptOptions};
use super::{types::{EvalKernel,EvalKernelFuture,EvalLanguage,EvalRuntimeInfo,EvalToolInput},detached_cell_manager::EvalDetachedCellManager,cell_handler::CellCompletionHandler,image_resize::EvalImageSdk};

pub trait EvalKernelManager: Send + Sync {
    fn get_kernel(&self,language:EvalLanguage) -> EvalKernelFuture<'_,Arc<dyn EvalKernel>>;
    // Managers without an HTTP completion bridge need no context registration.
    fn set_invocation_context(&self,_cell_id:&str,_context:EvalInvocationContext) -> Option<Box<dyn FnOnce()+Send>> {None}
    fn execution_tracker(&self) -> Option<&crate::extension::session_manager_proxy::SessionManagerProxy> { None }
}
pub type CellUpdateCallback=Arc<dyn Fn(AgentToolResult)+Send+Sync>;
pub type CellSettledCallback=Arc<dyn Fn(serde_json::Value)+Send+Sync>;
#[derive(Clone)]
pub struct EvalInvocationContext {
    pub model: Option<maho_ai::model::Model>,
    pub cwd: PathBuf,
    pub thinking_level: Option<maho_ai::types::ThinkingLevel>,
    pub goal_store_file: Option<PathBuf>,
}
pub struct CreateEvalToolOptions {
    pub kernel_manager: Arc<dyn EvalKernelManager>,
    pub executor: Arc<dyn OutputExecuteTool>,
    pub list_tools: Option<EvalToolCatalog>,
    pub complete: Option<CellCompletionHandler>,
    pub settings: CodemodeSettings,
    pub artifacts_dir: Option<PathBuf>,
    pub image_sdk: Arc<dyn EvalImageSdk>,
    pub cell_manager: Arc<Mutex<EvalDetachedCellManager>>,
    pub on_cell_settled: Option<CellSettledCallback>,
    pub prompt: EvalPromptOptions,
    pub runtimes: HashMap<EvalLanguage,EvalRuntimeInfo>,
    pub mode: String,
}
pub struct EvalCellInvocation {
    pub cell_id: String,
    pub input: EvalToolInput,
    pub signal: AbortSignal,
    pub on_update: Option<CellUpdateCallback>,
    pub mode: String,
    pub model: Option<maho_ai::model::Model>,
    pub context: Option<EvalInvocationContext>,
}
