//! The `<builtin:skills>` extension: bundled root, disabled filtering, `skill` tool, `/skill`.

use crate::{
    TOOL_NAME, bundled_skills_root, detail_text, disabled_skills, filter_prompt, list_text,
    run_skill_tool,
};
use maho_ext_api::{
    AgentToolResult, BeforeAgentStartEventResult, EventKind, EventResult, Extension, ExtensionApi,
    ExtensionEvent, NotificationType, ResourcesDiscoverResult, ToolDefinition, ToolError,
};
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;

/// The skills extension. `bundled_root: None` resolves the root from the process
/// ([`bundled_skills_root`]) at registration.
#[derive(Clone, Debug, Default)]
pub struct SkillsExtension {
    pub bundled_root: Option<PathBuf>,
}

impl SkillsExtension {
    /// The extension with the bundled root of this process.
    pub fn from_env() -> Self {
        Self {
            bundled_root: bundled_skills_root(),
        }
    }
}

fn definition() -> ToolDefinition {
    let mut definition = ToolDefinition::new(
        TOOL_NAME,
        "Load an available skill by name: returns its SKILL.md instructions (front matter removed) together with the skill directory that its relative paths resolve against. Pass `<skill>/<relative path>` to read a file or list a directory inside the skill (no absolute paths, no `..`). Optional `arguments` are appended as the user's extra instructions.",
        json!({
            "type": "object",
            "properties": {
                "name": {"type": "string", "description": "Skill name from the available skills list, optionally followed by /<relative path> inside the skill directory"},
                "arguments": {"type": "string", "description": "Extra instructions for the skill (optional)"}
            },
            "required": ["name"]
        }),
        Arc::new(|_| {
            Box::pin(async { Err(ToolError::Message("Extension context required".into())) })
        }),
    );
    "Skill".clone_into(&mut definition.label);
    definition.prompt_snippet = Some("Load a skill's instructions or one of its files".into());
    definition.prompt_guidelines = Some(vec![
        "Load a skill with the skill tool whenever its description matches the task; resolve the skill's relative paths against the skill directory it reports.".into(),
    ]);
    definition
}

impl Extension for SkillsExtension {
    fn register(&self, api: &mut ExtensionApi) {
        let bundled = self.bundled_root.clone();
        if let Err(error) = api.register_tool_with_extension_context(
            definition(),
            Arc::new(|_id, params, _signal, _update, ctx| {
                Box::pin(async move {
                    let skills = ctx.get_system_prompt_options().skills;
                    let disabled = disabled_skills(&ctx.cwd);
                    let outcome = run_skill_tool(&skills, &disabled, &params);
                    let mut result = AgentToolResult::text(outcome.text);
                    result.details = outcome.details;
                    if outcome.is_error {
                        result.is_error = Some(true);
                    }
                    Ok(result)
                })
            }),
        ) {
            maho_ext_api::diagnostic(&format!("skills: {error}"));
        }

        let discover_root = bundled.clone();
        api.on(
            EventKind::ResourcesDiscover,
            Arc::new(move |_event, _ctx| {
                let root = discover_root.clone();
                Box::pin(async move {
                    Ok(match root {
                        Some(root) => EventResult::ResourcesDiscover(ResourcesDiscoverResult {
                            skill_paths: vec![root.to_string_lossy().into_owned().into()],
                            ..Default::default()
                        }),
                        None => EventResult::None,
                    })
                })
            }),
        );

        api.on(
            EventKind::BeforeAgentStart,
            Arc::new(|event, ctx| {
                Box::pin(async move {
                    let ExtensionEvent::BeforeAgentStart(event) = event else {
                        return Ok(EventResult::None);
                    };
                    let disabled = disabled_skills(&ctx.cwd);
                    Ok(match filter_prompt(&event.system_prompt, &disabled) {
                        Some(system_prompt) => {
                            EventResult::BeforeAgentStart(BeforeAgentStartEventResult {
                                message: None,
                                system_prompt: Some(system_prompt),
                            })
                        }
                        None => EventResult::None,
                    })
                })
            }),
        );

        api.register_command(
            "skill",
            Some("List the loaded skills, or show one (/skill <name>)".into()),
            Some("[name]".into()),
            Arc::new(move |args, ctx| {
                let bundled = bundled.clone();
                Box::pin(async move {
                    let skills = ctx.get_system_prompt_options().skills;
                    let disabled = disabled_skills(&ctx.cwd);
                    let name = args.trim();
                    let text = if name.is_empty() {
                        list_text(&skills, &disabled, bundled.as_deref())
                    } else {
                        detail_text(&skills, &disabled, bundled.as_deref(), name)
                    };
                    ctx.ui.notify(&text, NotificationType::Info);
                    Ok(())
                })
            }),
        );
    }
}
