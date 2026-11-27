//! Port of senpi `packages/agent/src/execution/prepared/executor-pipeline.ts`.

use std::collections::HashMap;
use std::sync::Arc;

use maho_ai::types::BoxFuture;
use maho_ai::utils::abort::AbortSignal;
use serde_json::Value;

use crate::execution::tool_host::AfterToolFreeze;
use crate::tool_name_alias::resolve_tool_name_alias;
use crate::types::{
    AfterToolCall, AfterToolCallContext, AfterToolCallResult, AgentContext, AgentTool,
    AgentToolCall, BeforeToolCall, BeforeToolCallContext, BeforeToolCallResult,
    ResolveUnknownToolCall,
};

use super::executor_types::PreparationPipeline;

#[derive(Clone)]
pub struct PreparationSource {
    pub resolve_unknown_tool_call: Option<ResolveUnknownToolCall>,
    pub before_tool_call: Option<BeforeToolCall>,
    pub after_tool_call: Option<AfterToolCall>,
    pub removed_tool_hints: Option<HashMap<String, String>>,
    pub after_tool_freeze: AfterToolFreeze,
}

struct AgentPreparationPipeline {
    source: PreparationSource,
}

impl PreparationPipeline for AgentPreparationPipeline {
    fn resolve(
        &self,
        call: &AgentToolCall,
        context: &AgentContext,
    ) -> BoxFuture<'_, Result<Option<AgentTool>, String>> {
        let call_name = call.name.clone();
        let tools = context.tools.clone().unwrap_or_default();
        let context_cloned = context.clone();
        let resolver = self.source.resolve_unknown_tool_call.clone();
        Box::pin(async move {
            if let Some(exact) = tools.iter().find(|tool| tool.name() == call_name) {
                return Ok(Some(exact.clone()));
            }
            if let Some(resolver) = resolver
                && let Some(resolved) = resolver(call_name.clone(), context_cloned).await
            {
                return Ok(Some(resolved));
            }
            let available: Vec<String> = tools.iter().map(|t| t.name().to_owned()).collect();
            let Some(aliased) = resolve_tool_name_alias(&call_name, available) else {
                return Ok(None);
            };
            Ok(tools.into_iter().find(|tool| tool.name() == aliased))
        })
    }

    fn before_tool_call(
        &self,
        context: BeforeToolCallContext,
        signal: Option<AbortSignal>,
    ) -> BoxFuture<'_, Result<Option<BeforeToolCallResult>, String>> {
        let hook = self.source.before_tool_call.clone();
        Box::pin(async move {
            match hook {
                Some(hook) => Ok(hook(context, signal).await),
                None => Ok(None),
            }
        })
    }

    fn after_tool_call(
        &self,
        context: AfterToolCallContext,
        signal: Option<AbortSignal>,
    ) -> BoxFuture<'_, Result<Option<AfterToolCallResult>, String>> {
        let hook = self.source.after_tool_call.clone();
        Box::pin(async move {
            match hook {
                Some(hook) => Ok(hook(context, signal).await),
                None => Ok(None),
            }
        })
    }

    fn removed_tool_hint(&self, name: &str) -> Option<String> {
        self.source
            .removed_tool_hints
            .as_ref()
            .and_then(|hints| hints.get(name).cloned())
    }

    fn after_freeze(&self, prepared: &Value, frozen: &mut Value) {
        (self.source.after_tool_freeze)(prepared, frozen);
    }
}

pub fn agent_preparation_pipeline(agent: PreparationSource) -> Arc<dyn PreparationPipeline> {
    Arc::new(AgentPreparationPipeline { source: agent })
}
