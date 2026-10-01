//! Port of senpi packages/agent/src/harness/tools/tool-context.ts.
use crate::harness::types::ExecutionEnv;
use maho_ai::{types::BoxFuture, utils::abort::AbortSignal};
use std::sync::Arc;

#[derive(Clone)]
pub struct PostMutateContext {
    pub tool: MutationTool,
    pub path: String,
    pub signal: Option<AbortSignal>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationTool {
    Write,
    Edit,
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PostMutateResult {
    pub changed: bool,
    pub note: Option<String>,
}
pub type PostMutateHook = Arc<
    dyn Fn(PostMutateContext) -> BoxFuture<'static, Result<PostMutateResult, String>> + Send + Sync,
>;
#[derive(Clone)]
pub struct ExecutionToolContext {
    pub env: Arc<dyn ExecutionEnv>,
    pub post_mutate: Option<PostMutateHook>,
}
/// Allows callers to carry additional turn-specific data, like TS's generic context.
pub trait HasExecutionToolContext: Clone + Send + Sync + 'static {
    fn execution_tool_context(&self) -> &ExecutionToolContext;
}
impl HasExecutionToolContext for ExecutionToolContext {
    fn execution_tool_context(&self) -> &ExecutionToolContext {
        self
    }
}
