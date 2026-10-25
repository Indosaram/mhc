use senpi_task::agents::{
    AgentDefinition, BUILTIN_AGENT_DEFAULTS, curated_readonly_agent_names, map_omo_config_agents,
};
use serde_json::Value;
use std::collections::BTreeMap;

pub const TASK_CHILD_UI_ONLY_TOOL_NAMES: [&str; 2] = ["memory", "memory_apply_patch"];
pub struct InProcessRunnerBuildContext {
    pub shared_parent_tools: Vec<senpi_task::runners::in_process::shared_tool_filter::ChildToolRef>,
    pub max_depth: u32,
    pub create_session: senpi_task::runners::in_process::runner::CreateChildSession,
    pub parent_registry: senpi_task::manager::parent_registry_context::ParentModelRegistryResolver,
    /// omo.json `exclude_models`: an in-process child respawned or resumed on an excluded model fails
    /// model_unavailable instead of running it.
    pub exclude_models: Vec<String>,
}

/// `withoutExcludedSelectors`: a fallback chain entry kept out by `exclude_models` never reaches the
/// child, exactly as it would never be selected in the first place.
pub fn without_excluded_selectors(
    models: &[senpi_task::state::ResolvedModelRecord],
    exclude: &[String],
) -> Vec<senpi_task::state::ResolvedModelRecord> {
    models
        .iter()
        .filter(|model| {
            !senpi_task::model_exclusion::is_excluded_model(
                &format!("{}/{}", model.provider, model.model_id),
                exclude,
            )
        })
        .cloned()
        .collect()
}

/// `withoutExcludedRpcFallbacks`: the spec's fallback selectors with every excluded entry dropped.
pub fn without_excluded_fallbacks(
    spec: &senpi_task::manager::types::ManagedStartSpec,
    exclude: &[String],
) -> senpi_task::manager::types::ManagedStartSpec {
    let Some(models) = &spec.fallback_models else {
        return spec.clone();
    };
    senpi_task::manager::types::ManagedStartSpec {
        fallback_models: Some(without_excluded_selectors(models, exclude)),
        ..spec.clone()
    }
}

/// A runner that filters every start's and resume's fallback selectors before delegating, and forwards
/// the underlying runner's teardown settlement.
struct ExclusionFilteredRunner {
    inner: std::sync::Arc<dyn senpi_task::manager::types::ManagedRunner>,
    exclude: Vec<String>,
}

impl senpi_task::manager::types::ManagedRunner for ExclusionFilteredRunner {
    fn start(
        &self,
        spec: &senpi_task::manager::types::ManagedStartSpec,
    ) -> senpi_task::manager::types::ManagedRunnerResult {
        self.inner
            .start(&without_excluded_fallbacks(spec, &self.exclude))
    }
    fn resume(
        &self,
        spec: &senpi_task::manager::types::ManagedStartSpec,
        path: &str,
    ) -> Option<senpi_task::manager::types::ManagedRunnerResult> {
        self.inner
            .resume(&without_excluded_fallbacks(spec, &self.exclude), path)
    }
    fn settle_teardowns(&self) -> Result<(), senpi_task::manager::types::ManagedRunnerError> {
        self.inner.settle_teardowns()
    }
    fn start_staged(
        &self,
        spec: &senpi_task::manager::types::ManagedStartSpec,
    ) -> Option<
        Result<
            senpi_task::manager::types::StagedManagedChild,
            senpi_task::manager::types::ManagedRunnerError,
        >,
    > {
        self.inner
            .start_staged(&without_excluded_fallbacks(spec, &self.exclude))
    }
}

/// `withoutExcludedFallbacks`: the same runner with every excluded fallback selector filtered out.
pub fn without_excluded_fallbacks_runner(
    inner: std::sync::Arc<dyn senpi_task::manager::types::ManagedRunner>,
    exclude: &[String],
) -> std::sync::Arc<dyn senpi_task::manager::types::ManagedRunner> {
    if exclude.is_empty() {
        return inner;
    }
    std::sync::Arc::new(ExclusionFilteredRunner {
        inner,
        exclude: exclude.to_vec(),
    })
}
pub fn build_in_process_runner(
    build: InProcessRunnerBuildContext,
) -> std::sync::Arc<dyn senpi_task::manager::types::ManagedRunner> {
    let runner = senpi_task::runners::in_process::InProcessRunner::new(
        senpi_task::runners::in_process::InProcessRunnerOptions {
            shared_parent_tools: build.shared_parent_tools,
            ui_only_tool_names: TASK_CHILD_UI_ONLY_TOOL_NAMES
                .iter()
                .map(|name| (*name).into())
                .collect(),
            max_depth: Some(build.max_depth.saturating_add(1).max(1)),
            create_session: build.create_session,
        },
    );
    let runner=senpi_task::manager::runner::create_in_process_managed_runner(runner,senpi_task::manager::parent_registry_context::create_parent_registry_session_context_with_exclusions(build.parent_registry,build.exclude_models.clone()));
    without_excluded_fallbacks_runner(runner, &build.exclude_models)
}
pub type SharedParentTools = std::sync::Arc<
    dyn Fn() -> Vec<senpi_task::runners::in_process::shared_tool_filter::ChildToolRef>
        + Send
        + Sync,
>;
pub struct LiveInProcessRunnerBuildContext {
    pub shared_parent_tools: SharedParentTools,
    pub max_depth: u32,
    pub create_session: senpi_task::runners::in_process::runner::CreateChildSession,
    pub parent_registry: senpi_task::manager::parent_registry_context::ParentModelRegistryResolver,
    pub exclude_models: Vec<String>,
}
struct LiveInProcessRunner(LiveInProcessRunnerBuildContext);
impl LiveInProcessRunner {
    fn current(&self) -> std::sync::Arc<dyn senpi_task::manager::types::ManagedRunner> {
        build_in_process_runner(InProcessRunnerBuildContext {
            shared_parent_tools: (self.0.shared_parent_tools)(),
            max_depth: self.0.max_depth,
            create_session: self.0.create_session.clone(),
            parent_registry: self.0.parent_registry.clone(),
            exclude_models: self.0.exclude_models.clone(),
        })
    }
}
impl senpi_task::manager::types::ManagedRunner for LiveInProcessRunner {
    fn start_staged(
        &self,
        spec: &senpi_task::manager::types::ManagedStartSpec,
    ) -> Option<
        Result<
            senpi_task::manager::types::StagedManagedChild,
            senpi_task::manager::types::ManagedRunnerError,
        >,
    > {
        self.current().start_staged(spec)
    }
    fn settle_teardowns(&self) -> Result<(), senpi_task::manager::types::ManagedRunnerError> {
        self.current().settle_teardowns()
    }

    fn start(
        &self,
        spec: &senpi_task::manager::types::ManagedStartSpec,
    ) -> senpi_task::manager::types::ManagedRunnerResult {
        self.current().start(spec)
    }
    fn resume(
        &self,
        spec: &senpi_task::manager::types::ManagedStartSpec,
        path: &str,
    ) -> Option<senpi_task::manager::types::ManagedRunnerResult> {
        self.current().resume(spec, path)
    }
}
pub fn build_live_in_process_runner(
    build: LiveInProcessRunnerBuildContext,
) -> std::sync::Arc<dyn senpi_task::manager::types::ManagedRunner> {
    std::sync::Arc::new(LiveInProcessRunner(build))
}
/// The combined in-process + process runner set the task engine needs, over a live shared-parent
/// tool provider when the host supplies one (the parent session's tools grow during a run).
pub struct TaskRunnerBuildOptions {
    pub shared_parent_tools: Vec<senpi_task::runners::in_process::shared_tool_filter::ChildToolRef>,
    pub get_shared_parent_tools: Option<SharedParentTools>,
    pub max_depth: u32,
    pub create_session: senpi_task::runners::in_process::runner::CreateChildSession,
    pub parent_registry: senpi_task::manager::parent_registry_context::ParentModelRegistryResolver,
    pub rpc_options: senpi_task::runners::rpc_process::RpcProcessRunnerOptions,
    /// omo.json `exclude_models`, honored by every in-process and process child this build creates.
    pub exclude_models: Vec<String>,
}
pub fn build_task_runners(
    build: TaskRunnerBuildOptions,
) -> senpi_task::manager::types::ManagedRunners {
    let TaskRunnerBuildOptions {
        shared_parent_tools,
        get_shared_parent_tools,
        max_depth,
        create_session,
        parent_registry,
        rpc_options,
        exclude_models,
    } = build;
    let in_process = match get_shared_parent_tools {
        Some(shared_parent_tools) => {
            build_live_in_process_runner(LiveInProcessRunnerBuildContext {
                shared_parent_tools,
                max_depth,
                create_session,
                parent_registry,
                exclude_models: exclude_models.clone(),
            })
        }
        None => build_in_process_runner(InProcessRunnerBuildContext {
            shared_parent_tools,
            max_depth,
            create_session,
            parent_registry,
            exclude_models: exclude_models.clone(),
        }),
    };
    let process =
        without_excluded_fallbacks_runner(build_process_runner(rpc_options), &exclude_models);
    senpi_task::manager::types::ManagedRunners {
        in_process,
        process,
    }
}
pub fn build_process_runner(
    options: senpi_task::runners::rpc_process::RpcProcessRunnerOptions,
) -> std::sync::Arc<dyn senpi_task::manager::types::ManagedRunner> {
    senpi_task::manager::runner::create_rpc_managed_runner(
        senpi_task::runners::rpc_process::RpcProcessRunner::new(options),
    )
}
struct ConfiguredRpcRespawn(senpi_task::runners::rpc_process::RpcProcessRunner);
impl senpi_task::manager::types::RpcRespawnRunner for ConfiguredRpcRespawn {
    fn start(
        &self,
        spec: &senpi_task::runners::types::RpcRunnerSpec,
    ) -> senpi_task::manager::types::ManagedRunnerResult {
        // RpcProcessRunner applies current exclusions to rebuilt decorated selectors.
        self.0
            .start(spec)
            .map(|handle| handle as std::sync::Arc<dyn senpi_task::manager::ManagedChildHandle>)
            .map_err(senpi_task::manager::types::ManagedRunnerError::Runner)
    }
}
pub fn build_rpc_respawn_runner(
    options: senpi_task::runners::rpc_process::RpcProcessRunnerOptions,
) -> std::sync::Arc<dyn senpi_task::manager::types::RpcRespawnRunner> {
    std::sync::Arc::new(ConfiguredRpcRespawn(
        senpi_task::runners::rpc_process::RpcProcessRunner::new(options),
    ))
}
/// The respawn seam as the engine builds it: the excluded models are enforced on the manager's start
/// and resume paths, which is where a revived child's chain is filtered.
pub fn build_rpc_respawn_runner_with_exclusions(
    mut options: senpi_task::runners::rpc_process::RpcProcessRunnerOptions,
    exclude_models: Vec<String>,
) -> std::sync::Arc<dyn senpi_task::manager::types::RpcRespawnRunner> {
    options.exclude_models = Some(std::sync::Arc::new(move || exclude_models.clone()));
    build_rpc_respawn_runner(options)
}

pub fn resolve_task_agents(config: &Value) -> BTreeMap<String, AgentDefinition> {
    resolve_task_agents_with_definitions(config, BTreeMap::new())
}

pub fn resolve_task_agents_with_compat(
    config: &Value,
    cwd: &std::path::Path,
    claude_dir: &std::path::Path,
) -> BTreeMap<String, AgentDefinition> {
    let definitions = match claude_code_compat_core::load_native_agents(cwd, claude_dir, None) {
        Ok(definitions) => definitions,
        Err(error) => {
            maho_ext_api::diagnostic(&format!("Claude agent loading failed: {error}"));
            BTreeMap::new()
        }
    };
    resolve_task_agents_with_definitions(config, definitions)
}

fn resolve_task_agents_with_definitions(
    config: &Value,
    definitions: BTreeMap<String, claude_code_compat_core::ClaudeCodeAgentConfig>,
) -> BTreeMap<String, AgentDefinition> {
    let mut merged: BTreeMap<_, _> = BUILTIN_AGENT_DEFAULTS
        .iter()
        .map(|definition| (definition.name.clone(), definition.clone()))
        .collect();
    for (name, agent) in definitions {
        use claude_code_compat_core::{AgentMode, AgentModel};
        let definition = merged
            .entry(name.clone())
            .or_insert_with(|| AgentDefinition::named(&name));
        if agent.description.is_some() {
            definition.description = agent.description;
        }
        if agent.prompt.is_some() {
            definition.prompt = agent.prompt;
        }
        if let Some(mode) = agent.mode {
            definition.mode = Some(
                match mode {
                    AgentMode::Subagent => "subagent",
                    AgentMode::Primary => "primary",
                    AgentMode::All => "all",
                }
                .into(),
            );
        }
        if let Some(model) = agent.model {
            definition.model = Some(match model {
                AgentModel::String(model) => model,
                AgentModel::Provider {
                    provider_id,
                    model_id,
                } => format!("{provider_id}/{model_id}"),
            });
        }
        if let Some(tools) = agent.tools {
            definition.tools = Some(
                tools
                    .into_iter()
                    .map(|(name, allow)| senpi_task::agents::AgentToolRule::new(&name, allow))
                    .collect(),
            );
        }
    }
    for (name, overlay) in map_omo_config_agents(config) {
        let definition = merged
            .entry(name)
            .or_insert_with(|| AgentDefinition::named(&overlay.name));
        macro_rules! overlay_fields { ($($field:ident),* $(,)?) => { $(if overlay.$field.is_some() { definition.$field = overlay.$field; })* }; }
        overlay_fields!(
            description,
            prompt,
            mode,
            model,
            models,
            variant,
            reasoning_effort,
            temperature,
            tools,
            disable,
            background,
            execution_mode,
            allowed_subagents,
            disallowed_tools,
            max_depth,
            max_turns
        );
    }
    for name in curated_readonly_agent_names() {
        if let Some(definition) = merged.get_mut(name) {
            definition.execution_mode = Some("in-process".into());
        }
    }
    merged
}
