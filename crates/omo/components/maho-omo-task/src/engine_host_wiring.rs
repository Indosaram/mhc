//! `engine-host-wiring.ts`: thread `omo.json` `exclude_models` into every runner build context.
//!
//! The half of the engine that reaches a task host: the lifecycle that revives recorded host sessions,
//! and the runner context whose shard routing the `process` runners open children with. The lifecycle
//! and the runners share ONE host runtime, so a revival ensure and a spawn ensure hit the same cache.
//!
//! The one behavior this module owns on its own is the exclusion thread: a build context that lost
//! `exclude_models` lets a child run a model the user keeps out, so the wiring reads it from the
//! config exactly once and copies it into every context it hands out.

use std::sync::Arc;

use serde_json::Value;

/// The runner build context the wiring produces (`RunnerBuildContext`).
#[derive(Clone)]
pub struct RunnerBuildContext {
    pub shared_parent_tools: crate::engine_runners::SharedParentTools,
    pub max_depth: u32,
    pub create_session: senpi_task::runners::in_process::runner::CreateChildSession,
    pub parent_registry: senpi_task::manager::parent_registry_context::ParentModelRegistryResolver,
    /// omo.json `exclude_models`, read once from the composed config.
    pub exclude_models: Vec<String>,
    /// The session's shared package-aware inherited list: every child-launch producer resolves the
    /// SAME list (#8492).
    pub resolve_inherited_extensions: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
}

/// `omoConfig.exclude_models` as the runner contexts consume it.
pub fn exclude_models_of(config: &Value) -> Vec<String> {
    config
        .get("exclude_models")
        .and_then(Value::as_array)
        .map(|patterns| patterns.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        .unwrap_or_default()
}

pub struct EngineHostWiringInput {
    pub config: Value,
    pub shared_parent_tools: crate::engine_runners::SharedParentTools,
    pub max_depth: u32,
    pub create_session: senpi_task::runners::in_process::runner::CreateChildSession,
    pub parent_registry: senpi_task::manager::parent_registry_context::ParentModelRegistryResolver,
}

/// `composeEngineHostWiring`: one context, with the exclusion list already threaded.
pub fn compose_engine_host_wiring(input: EngineHostWiringInput) -> RunnerBuildContext {
    RunnerBuildContext {
        shared_parent_tools: input.shared_parent_tools,
        max_depth: input.max_depth,
        create_session: input.create_session,
        parent_registry: input.parent_registry,
        exclude_models: exclude_models_of(&input.config),
        resolve_inherited_extensions: Arc::new(Vec::new),
    }
}

/// The runner build options this context feeds, with `exclude_models` carried through.
pub fn runner_build_options(context: &RunnerBuildContext, rpc_options: senpi_task::runners::rpc_process::RpcProcessRunnerOptions) -> crate::engine_runners::TaskRunnerBuildOptions {
    crate::engine_runners::TaskRunnerBuildOptions {
        shared_parent_tools: Vec::new(),
        get_shared_parent_tools: Some(context.shared_parent_tools.clone()),
        max_depth: context.max_depth,
        create_session: context.create_session.clone(),
        parent_registry: context.parent_registry.clone(),
        rpc_options,
        exclude_models: context.exclude_models.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_exclusion_list_is_read_once_from_the_config() {
        assert_eq!(
            exclude_models_of(&json!({"exclude_models": ["openai/gpt-5*", 7, "anthropic/*"]})),
            vec!["openai/gpt-5*".to_string(), "anthropic/*".to_string()]
        );
        assert!(exclude_models_of(&json!({})).is_empty());
    }
}
