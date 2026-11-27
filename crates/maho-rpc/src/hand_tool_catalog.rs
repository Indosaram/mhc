//! Port of senpi `packages/coding-agent/src/modes/rpc/hand-tool-catalog.ts` and
//! `packages/coding-agent/src/modes/hand/tool-catalog.ts`.
//!
//! The tool list a hand's machine would show the model of a local session there, for a loop that
//! runs elsewhere and runs its tools on this hand (`senpi#6749`, S-CAT).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use maho_core::runtime::active_tools::{
    armed_eval_only_tool_names, direct_active_tool_names, is_activated_on_registration,
    registered_eval_only_tool_names, resolve_eval_only_tool_names,
};
use maho_core::runtime::bootstrap::ToolRegistryBuild;
use maho_core::skills::Skill;
use maho_core::system_prompt::ContextFile;
use maho_ext_api::{ToolDefinition, ToolExposure};
use maho_tools::definition::KernelPrelude;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::loopless_loop_hooks::LooplessLoopHooks;

const DESCRIBE_TOOLS_EVENT: &str = "describe_tools";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDeclaration {
    pub name: String,
    pub label: String,
    pub description: String,
    pub parameters: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_snippet: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_guidelines: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kernel_prelude: Option<KernelPrelude>,
    pub session_bound: bool,
    pub exposure: ToolExposure,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search_text: Option<String>,
    pub search_keywords: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search_group: Option<String>,
    pub allow_lazy_activation: bool,
}

pub fn tool_catalog<'a>(
    tools: impl IntoIterator<Item = &'a ToolDefinition>,
) -> Vec<ToolDeclaration> {
    tools
        .into_iter()
        .map(|tool| {
            let exposure = tool.exposure.unwrap_or(ToolExposure::Direct);
            ToolDeclaration {
                name: tool.name.clone(),
                label: tool.label.clone(),
                description: tool.description.clone(),
                parameters: tool.parameters.clone(),
                prompt_snippet: tool.prompt_snippet.clone(),
                prompt_guidelines: tool.prompt_guidelines.clone(),
                kernel_prelude: tool.kernel_prelude.clone(),
                session_bound: false,
                exposure,
                search_text: if exposure == ToolExposure::Search {
                    tool.search_text.clone()
                } else {
                    None
                },
                search_keywords: tool.search_keywords.clone().unwrap_or_default(),
                search_group: tool.search_group.clone(),
                allow_lazy_activation: tool.allow_lazy_activation != Some(false),
            }
        })
        .collect()
}

pub fn hand_tool_catalog(registry: &ToolRegistryBuild) -> Vec<ToolDeclaration> {
    tool_catalog(registry.definitions.values().map(|entry| &entry.definition))
}

/// Prompt resources a hand shares for a scope's cwd.
pub trait HandCatalogResources: Send + Sync {
    fn context_files(&self) -> Vec<ContextFile>;
    fn skills(&self) -> Vec<Skill>;
}

/// What a runtime knows that a session on the same cwd, flags and settings would decide its tool
/// list from.
pub struct HandToolCatalogSource {
    pub registry: Arc<ToolRegistryBuild>,
    pub initial_active_tool_names: Vec<String>,
    pub allowed_tool_names: Option<HashSet<String>>,
    pub resources: Arc<dyn HandCatalogResources>,
}

fn requested_active_names(
    registry: &ToolRegistryBuild,
    definitions: &HashMap<String, ToolDefinition>,
    initial_active_tool_names: &[String],
    allowed_tool_names: Option<&HashSet<String>>,
) -> Vec<String> {
    let mut names = Vec::new();
    let mut seen = HashSet::new();
    for name in initial_active_tool_names {
        if registry.is_allowed_tool(name) && seen.insert(name.clone()) {
            names.push(name.clone());
        }
    }
    if let Some(allowed) = allowed_tool_names {
        for name in registry.tool_registry.keys() {
            if allowed.contains(name) && seen.insert(name.clone()) {
                names.push(name.clone());
            }
        }
    } else {
        for tool in &registry.extension_tools {
            if is_activated_on_registration(definitions, &tool.name)
                && seen.insert(tool.name.clone())
            {
                names.push(tool.name.clone());
            }
        }
    }
    names
}

pub fn describe_hand_tools(source: &HandToolCatalogSource) -> Value {
    let registry = &source.registry;
    let defs: HashMap<String, ToolDefinition> = registry
        .definitions
        .iter()
        .map(|(k, v)| (k.clone(), v.definition.clone()))
        .collect();
    let registered: HashSet<String> = registry.tool_registry.keys().cloned().collect();
    let requested = requested_active_names(
        registry,
        &defs,
        &source.initial_active_tool_names,
        source.allowed_tool_names.as_ref(),
    );
    let eval_only_resolved = resolve_eval_only_tool_names(&defs, None);
    let armed = armed_eval_only_tool_names(Some(&eval_only_resolved), &registered);
    let active_split = direct_active_tool_names(&requested, &defs, &registered, armed.as_ref());
    let mut eval_only: Vec<String> = registered_eval_only_tool_names(armed.as_ref(), &registered)
        .into_iter()
        .collect();
    eval_only.sort();

    let context_files: Vec<Value> = source
        .resources
        .context_files()
        .into_iter()
        .map(|f| json!({ "path": f.path, "content": f.content }))
        .collect();
    let skills: Vec<Value> = source
        .resources
        .skills()
        .into_iter()
        .map(|s| {
            json!({
                "name": s.name,
                "description": s.description,
                "filePath": s.file_path,
                "baseDir": s.base_dir,
                "disableModelInvocation": s.disable_model_invocation,
            })
        })
        .collect();

    json!({
        "tools": hand_tool_catalog(registry),
        "active": active_split.active,
        "evalOnly": eval_only,
        "context": {
            "contextFiles": context_files,
            "skills": skills,
        },
    })
}

/// `hooks`, answering `describe_tools` itself: the catalog is no extension loop hook (no handler
/// runs and nothing is sent), so it is served by any hand, whether or not it is its extensions'
/// home.
pub fn with_tool_catalog(
    hooks: LooplessLoopHooks,
    source: Arc<HandToolCatalogSource>,
) -> LooplessLoopHooks {
    Arc::new(move |event, payload| {
        let hooks = Arc::clone(&hooks);
        let source = Arc::clone(&source);
        Box::pin(async move {
            if event != DESCRIBE_TOOLS_EVENT {
                return hooks(event, payload).await;
            }
            let Some(obj) = payload.as_object() else {
                return Err(format!("malformed {DESCRIBE_TOOLS_EVENT} payload"));
            };
            let has_session_id = obj.get("sessionId").and_then(Value::as_str).is_some();
            let has_idle = obj.get("idle").and_then(Value::as_bool).is_some();
            if !has_session_id || !has_idle {
                return Err(format!("malformed {DESCRIBE_TOOLS_EVENT} payload"));
            }
            Ok(describe_hand_tools(&source))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_agent::types::{AgentTool, AgentToolResult};
    use maho_ai::types::Tool;
    use maho_core::runtime::bootstrap::ToolDefinitionEntry;
    use maho_ext_api::SourceInfo;

    struct EmptyResources;
    impl HandCatalogResources for EmptyResources {
        fn context_files(&self) -> Vec<ContextFile> {
            Vec::new()
        }
        fn skills(&self) -> Vec<Skill> {
            Vec::new()
        }
    }

    fn fake_tool_def(name: &str, exposure: Option<ToolExposure>) -> ToolDefinition {
        ToolDefinition {
            name: name.to_owned(),
            label: name.to_owned(),
            description: format!("tool {name}"),
            exposure,
            search_text: Some("search".to_owned()),
            search_keywords: None,
            search_group: None,
            allow_lazy_activation: None,
            prompt_snippet: None,
            prompt_guidelines: None,
            parameters: json!({"type": "object"}),
            output_schema: None,
            freeform: None,
            constrained_sampling: None,
            render_shell: None,
            prepare_arguments: None,
            workspace_effect: None,
            execution_mode: None,
            execute: Arc::new(|_| {
                Box::pin(async { Ok(maho_tools::definition::ToolResult::text("ok")) })
            }),
            kernel_prelude: None,
            permission_parser: None,
            dispatch_identity: None,
            server_pinned: None,
            placement: None,
            child_job: None,
        }
    }

    fn fake_agent_tool(name: &str) -> AgentTool {
        AgentTool {
            label: name.to_owned(),
            prepare_arguments: None,
            execute: Arc::new(|_, _, _, _| Box::pin(async { AgentToolResult::text("ok") })),
            replay: None,
            execution_mode: None,
            tool: Tool {
                name: name.to_owned(),
                description: format!("tool {name}"),
                parameters: json!({"type": "object"}),
                freeform: None,
                output_schema: None,
                constrained_sampling: None,
            },
        }
    }

    #[tokio::test]
    async fn describes_catalog_and_intercepts_describe_tools_event() {
        let mut build = ToolRegistryBuild::default();
        let read_def = fake_tool_def("read", None);
        let search_def = fake_tool_def("web", Some(ToolExposure::Search));
        build.definitions.insert(
            "read".into(),
            ToolDefinitionEntry {
                definition: read_def,
                source_info: SourceInfo::default(),
            },
        );
        build.definitions.insert(
            "web".into(),
            ToolDefinitionEntry {
                definition: search_def,
                source_info: SourceInfo::default(),
            },
        );
        build
            .tool_registry
            .insert("read".into(), fake_agent_tool("read"));
        build
            .tool_registry
            .insert("web".into(), fake_agent_tool("web"));

        let source = Arc::new(HandToolCatalogSource {
            registry: Arc::new(build),
            initial_active_tool_names: vec!["read".into()],
            allowed_tool_names: None,
            resources: Arc::new(EmptyResources),
        });

        let base_hooks: LooplessLoopHooks =
            Arc::new(|event, _| Box::pin(async move { Err(format!("unexpected hook {event}")) }));
        let wrapped = with_tool_catalog(base_hooks, source);

        let err = wrapped("describe_tools".into(), json!({"sessionId": "s"}))
            .await
            .expect_err("missing idle");
        assert!(err.contains("malformed"));

        let res = wrapped(
            "describe_tools".into(),
            json!({"sessionId": "s", "idle": true}),
        )
        .await
        .expect("catalog");
        assert_eq!(res["active"], json!(["read"]));
        assert_eq!(res["tools"].as_array().expect("tools").len(), 2);
    }
}
