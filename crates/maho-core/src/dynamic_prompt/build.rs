//! Port of senpi `packages/coding-agent/src/core/dynamic-prompt/build.ts`.

use std::collections::HashMap;

use super::identity::build_identity_section;
use super::intent_gate::build_intent_gate;
use super::policies::build_policies_section;
use super::style::build_style_section;
use super::tool_categorization::categorize_tools;
use super::tool_section::build_tool_section;
use super::types::{AvailableTool, PromptSurface};
use super::verification::build_verification_section;
use super::working_task::build_working_task_section;
use super::workstation::{
    BuildWorkstationSectionOptions, WorkstationDialect, build_workstation_section,
};
use crate::skills::{FileReadTool, Skill, format_skills_for_prompt};
use crate::system_prompt::{ContextFile, ContextFileScope};

const USER_INSTRUCTIONS_HEADER: &str = "\n\n## User Instructions\n\nThe user's own instruction files (below) are standing instructions from the user. They override the default working style above, including whether, when, and how to ask the user questions; only an explicit instruction in the current conversation outranks them.\n\n";

const PROJECT_CONTEXT_HEADER: &str = "\n\n## Project Context\n\nProject instruction files (below, and in [Directory Context: ...] blocks injected during reads) apply to work under their directory and override the default working style above, including whether, when, and how to ask the user questions; deeper files win on conflict; explicit user instructions override.\n\n";

/// `PROMPT_SURFACE_ENV_VAR`.
pub const PROMPT_SURFACE_ENV_VAR: &str = "SENPI_PROMPT_SURFACE";

/// `resolvePromptSurface(env)`: `SENPI_PROMPT_SURFACE=app` or `=chat` selects that surface; unset or any other
/// value is the terminal. `env` looks a variable up by name.
pub fn resolve_prompt_surface(env: impl Fn(&str) -> Option<String>) -> PromptSurface {
    match env(PROMPT_SURFACE_ENV_VAR).as_deref() {
        Some("app") => PromptSurface::App,
        Some("chat") => PromptSurface::Chat,
        _ => PromptSurface::Terminal,
    }
}

/// `DynamicPromptCoreContext`: what a `corePrompt` override may reuse.
pub struct DynamicPromptCoreContext {
    pub tools: Vec<AvailableTool>,
    pub tool_section: String,
    pub surface: PromptSurface,
}

pub type PromptPartsObserver<'a> = &'a dyn Fn(&[DynamicSystemPromptPart]);
/// `BuildDynamicSystemPromptOptions`.
pub struct BuildDynamicSystemPromptOptions<'a> {
    pub custom_prompt: Option<String>,
    pub append_system_prompt: Option<String>,
    pub on_parts: Option<PromptPartsObserver<'a>>,
    pub cwd: String,
    pub selected_tools: Vec<String>,
    pub tool_snippets: HashMap<String, String>,
    pub prompt_guidelines: Vec<String>,
    pub context_files: Vec<ContextFile>,
    pub skills: Vec<Skill>,
    pub tuning_section: Option<String>,
    pub core_prompt: Option<&'a dyn Fn(&DynamicPromptCoreContext) -> String>,
    pub workstation_dialect: Option<WorkstationDialect>,
    pub surface: PromptSurface,
}

pub use maho_ext_api::DynamicSystemPromptPart;

fn part(name: &str, source: &str, text: String) -> DynamicSystemPromptPart {
    DynamicSystemPromptPart { name: name.to_owned(), source: Some(source.to_owned()), text }
}

pub fn build_dynamic_system_prompt(options: &BuildDynamicSystemPromptOptions<'_>) -> String {
    let parts = build_dynamic_system_prompt_parts(options);
    if let Some(on_parts) = options.on_parts { on_parts(&parts); }
    parts.iter().map(|part| part.text.as_str()).collect()
}

pub fn build_dynamic_system_prompt_parts(options: &BuildDynamicSystemPromptOptions<'_>) -> Vec<DynamicSystemPromptPart> {
    if let Some(custom) = &options.custom_prompt {
        let mut parts = vec![part("custom_prompt", "loader", custom.clone())];
        if let Some(append) = &options.append_system_prompt { parts.push(part("append_system_prompt", "loader", format!("\n\n{append}"))); }
        return parts;
    }
    let tools = categorize_tools(&options.selected_tools);

    let tool_section =
        build_tool_section(&tools, &options.tool_snippets, &options.prompt_guidelines);

    let sections: Vec<String> = match options.core_prompt {
        Some(core_prompt) => vec![core_prompt(&DynamicPromptCoreContext {
            tools: tools.clone(),
            tool_section: tool_section.clone(),
            surface: options.surface,
        })],
        None => vec![
            build_identity_section(),
            String::new(),
            build_intent_gate(&tools),
            String::new(),
            build_working_task_section(),
            String::new(),
            build_verification_section(),
            String::new(),
            tool_section.clone(),
            String::new(),
            build_policies_section(),
            String::new(),
            super::handoff::build_handoff_section(options.surface, false),
            String::new(),
            build_style_section(),
        ],
    };

    let mut parts = vec![part("core", "dynamic-prompt", sections.join("\n"))];
    if let Some(tuning) = options.tuning_section.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        parts.push(part("tuning", "prompt-preset", format!("\n\n{tuning}")));
    }
    // Instruction files outrank the core's and presets' default working style, asking included.
    let user_files: Vec<&ContextFile> = options.context_files.iter().filter(|file| file.scope == Some(ContextFileScope::User)).collect();
    let project_files: Vec<&ContextFile> = options.context_files.iter().filter(|file| file.scope != Some(ContextFileScope::User)).collect();
    for (files, header) in [(user_files, USER_INSTRUCTIONS_HEADER), (project_files, PROJECT_CONTEXT_HEADER)] {
        if files.is_empty() { continue; }
        parts.push(part("project_context", "dynamic-prompt", header.to_owned()));
        for (index, file) in files.iter().enumerate() {
            let text = format!("### {}\n\n{}", file.path, file.content.trim_end());
            parts.push(part("project_context", &file.path, if index + 1 == files.len() { text.trim_end().to_owned() } else { format!("{text}\n\n") }));
        }
    }
    let skills = format_skills_for_prompt(&options.skills, FileReadTool::Read);
    if !skills.is_empty() { parts.push(part("skills", "skills", format!("\n{skills}"))); }
    let workstation = build_workstation_section(&BuildWorkstationSectionOptions {
        selected_tools: options.selected_tools.clone(), dialect: options.workstation_dialect, facts: None,
    });
    parts.push(part("workstation", "dynamic-prompt", format!("\n\n{workstation}")));
    if let Some(append) = &options.append_system_prompt { parts.push(part("append_system_prompt", "loader", format!("\n\n{append}"))); }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::Skill;
    use crate::source_info::{SyntheticSourceInfoOptions, create_synthetic_source_info};

    fn options() -> BuildDynamicSystemPromptOptions<'static> {
        let mut tool_snippets: HashMap<String, String> = HashMap::new();
        tool_snippets.insert("read".to_string(), "Read file contents".to_string());
        BuildDynamicSystemPromptOptions {
            custom_prompt: None, append_system_prompt: None, on_parts: None,
            cwd: "/work".to_string(),
            selected_tools: vec!["read".to_string()],
            tool_snippets,
            prompt_guidelines: Vec::new(),
            context_files: Vec::new(),
            skills: Vec::new(),
            tuning_section: None,
            core_prompt: None,
            workstation_dialect: None,
            surface: PromptSurface::Terminal,
        }
    }

    fn skill(name: &str) -> Skill {
        let file_path = format!("/skills/{name}/SKILL.md");
        Skill {
            name: name.to_string(),
            description: format!("{name} description"),
            base_dir: format!("/skills/{name}"),
            source_info: create_synthetic_source_info(
                &file_path,
                SyntheticSourceInfoOptions {
                    source: "local".to_string(),
                    ..Default::default()
                },
            ),
            file_path,
            disable_model_invocation: false,
            provenance: None,
            allowed_set: None,
        }
    }

    #[test]
    fn custom_and_appended_prompt_parts_are_exact() {
        let mut options = options();
        options.custom_prompt = Some("custom".into());
        options.append_system_prompt = Some("append".into());
        let parts = build_dynamic_system_prompt_parts(&options);
        assert_eq!(parts.iter().map(|part| part.text.as_str()).collect::<String>(), build_dynamic_system_prompt(&options));
        assert_eq!(parts[0].name, "custom_prompt");
        assert_eq!(parts[1].name, "append_system_prompt");
        assert_eq!(parts[0].source.as_deref(), Some("loader"));
    }

    #[test]
    fn the_default_sections_are_assembled_in_order() {
        let prompt = build_dynamic_system_prompt(&options());
        let identity = prompt.find("You are ").expect("identity");
        let intent = prompt.find("## Intent Gate").expect("intent");
        let working = prompt.find("## Working the Task").expect("working");
        let verification = prompt.find("## Verification").expect("verification");
        let tools = prompt.find("## Available Tools").expect("tools");
        let policies = prompt.find("## Policies").expect("policies");
        let style = prompt.find("## Style").expect("style");
        let workstation = prompt.find("<workstation>").expect("workstation");
        assert!(
            identity < intent && intent < working && working < verification && verification < tools
        );
        assert!(tools < policies && policies < style && style < workstation);
        assert!(prompt.ends_with(&build_workstation_section(
            &BuildWorkstationSectionOptions {
                selected_tools: options().selected_tools,
                dialect: None,
                facts: None,
            }
        )));
    }

    #[test]
    fn a_tuning_section_is_appended_after_style() {
        let mut options = options();
        options.tuning_section = Some("  model addendum  ".to_string());
        let prompt = build_dynamic_system_prompt(&options);
        let style = prompt.find("## Style").expect("style");
        let tuning = prompt.find("model addendum").expect("tuning");
        assert!(style < tuning);
        assert!(tuning < prompt.find("<workstation>").expect("workstation"));
    }

    #[test]
    fn a_blank_tuning_section_is_ignored() {
        let mut options = options();
        options.tuning_section = Some("   ".to_string());
        let prompt = build_dynamic_system_prompt(&options);
        assert!(!prompt.contains("## Style\n\n\n"));
        assert_eq!(prompt.matches("## Style").count(), 1);
    }

    #[test]
    fn context_files_render_as_a_project_context_section() {
        let mut options = options();
        options.context_files = vec![ContextFile {
            path: "/work/AGENTS.md".to_string(),
            content: "rules  \n".to_string(),
            scope: None,
        }];
        let prompt = build_dynamic_system_prompt(&options);
        assert!(prompt.contains("## Project Context"));
        assert!(prompt.contains("### /work/AGENTS.md\n\nrules\n"));
        assert!(prompt.contains("apply to work under their directory and override the default working style above"));
        assert!(!prompt.contains("## User Instructions"));
    }

    #[test]
    fn user_scoped_files_get_their_own_section_before_project_context() {
        let mut options = options();
        options.context_files = vec![
            ContextFile { path: "/work/AGENTS.md".to_string(), content: "project".to_string(), scope: None },
            ContextFile { path: "/home/u/AGENTS.md".to_string(), content: "mine".to_string(), scope: Some(ContextFileScope::User) },
        ];
        let prompt = build_dynamic_system_prompt(&options);
        let user = prompt.find("## User Instructions").expect("user section");
        let project = prompt.find("## Project Context").expect("project section");
        assert!(user < project);
        assert!(prompt.find("### /home/u/AGENTS.md").expect("user file") < project);
        assert!(prompt.find("### /work/AGENTS.md").expect("project file") > project);
    }

    #[test]
    fn skills_are_appended_before_the_workstation_block() {
        let mut options = options();
        options.skills = vec![skill("demo")];
        let prompt = build_dynamic_system_prompt(&options);
        let skills = prompt.find("<available_skills>").expect("skills");
        let workstation = prompt.find("<workstation>").expect("workstation");
        assert!(skills < workstation);
        assert!(prompt.contains("Use the read tool to load a skill's file"));
    }

    #[test]
    fn a_core_prompt_override_replaces_identity_through_style_but_keeps_the_rest() {
        let core =
            |context: &DynamicPromptCoreContext| format!("CUSTOM CORE\n{}", context.tool_section);
        let mut options = options();
        options.core_prompt = Some(&core);
        options.skills = vec![skill("demo")];
        let prompt = build_dynamic_system_prompt(&options);
        assert!(prompt.starts_with("CUSTOM CORE\n## Available Tools"));
        assert!(!prompt.contains("## Intent Gate"));
        assert!(!prompt.contains("## Style"));
        assert!(prompt.contains("<available_skills>"));
        assert!(prompt.contains("<workstation>"));
        assert!(!prompt.contains("Current working directory:"));
    }

    // senpi test/dynamic-prompt/build.test.ts cases not covered above.
    #[test]
    fn the_intent_gate_forces_verbalization() {
        let prompt = build_dynamic_system_prompt(&options());
        assert!(prompt.contains("I read this as"));
        assert!(!prompt.contains("Keep the routing decision internal"));
    }

    #[test]
    fn working_the_task_and_verification_carry_their_disciplines() {
        let prompt = build_dynamic_system_prompt(&options());
        assert!(prompt.contains("loosely relevant"));
        assert!(prompt.contains("Stop searching when"));
        for tier in ["V1", "V2", "V3"] {
            assert!(prompt.contains(tier), "{tier}");
        }
    }

    #[test]
    fn policies_are_language_agnostic_and_style_names_the_smallest_change() {
        let prompt = build_dynamic_system_prompt(&options());
        assert!(prompt.contains("Hard Blocks"));
        assert!(!prompt.contains("as any"));
        assert!(!prompt.contains("ts-ignore"));
        assert!(prompt.contains("Smallest correct change"));
    }

    #[test]
    fn renders_the_handoff_section_exactly_once() {
        assert_eq!(
            build_dynamic_system_prompt(&options())
                .matches("## Handoff")
                .count(),
            1
        );
    }

    #[test]
    fn no_tuning_section_by_default() {
        assert!(!build_dynamic_system_prompt(&options()).contains("## Model Notes"));
    }

    // senpi#2093: date and cwd reach the model as an environment-context message instead.
    #[test]
    fn carries_no_working_directory_and_is_byte_identical_across_directories() {
        let mut first = options();
        first.cwd = "/test/project".to_string();
        let prompt = build_dynamic_system_prompt(&first);
        assert!(!prompt.contains("Current date:"));
        assert!(!prompt.contains("Current working directory:"));
        assert!(!prompt.contains("/test/project"));
        let mut second = options();
        second.cwd = "C:\\Users\\test\\other".to_string();
        assert_eq!(build_dynamic_system_prompt(&second), prompt);
    }

    #[test]
    fn excludes_skills_with_disable_model_invocation() {
        let mut options = options();
        let mut hidden = skill("hidden-skill");
        hidden.disable_model_invocation = true;
        options.skills = vec![hidden];
        assert!(!build_dynamic_system_prompt(&options).contains("hidden-skill"));
    }

    #[test]
    fn includes_custom_prompt_guidelines() {
        let mut options = options();
        options.prompt_guidelines = vec!["Use read to examine files instead of cat or sed.".into()];
        assert!(
            build_dynamic_system_prompt(&options)
                .contains("Use read to examine files instead of cat or sed.")
        );
    }

    #[test]
    fn the_workstation_dialect_is_forwarded() {
        let mut options = options();
        options.workstation_dialect = Some(WorkstationDialect::Claude);
        let prompt = build_dynamic_system_prompt(&options);
        assert!(prompt.contains("<execution_context>"));
    }
}
