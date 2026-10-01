//! Port of senpi `packages/coding-agent/src/core/dynamic-prompt/build.ts`.

use std::collections::HashMap;

use super::identity::build_identity_section;
use super::intent_gate::build_intent_gate;
use super::policies::build_policies_section;
use super::style::build_style_section;
use super::tool_categorization::categorize_tools;
use super::tool_section::build_tool_section;
use super::types::AvailableTool;
use super::verification::build_verification_section;
use super::working_task::build_working_task_section;
use super::workstation::{BuildWorkstationSectionOptions, WorkstationDialect, build_workstation_section};
use crate::skills::{FileReadTool, Skill, format_skills_for_prompt};
use crate::system_prompt::ContextFile;

/// `DynamicPromptCoreContext`: what a `corePrompt` override may reuse.
pub struct DynamicPromptCoreContext {
    pub tools: Vec<AvailableTool>,
    pub tool_section: String,
}

/// `BuildDynamicSystemPromptOptions`.
pub struct BuildDynamicSystemPromptOptions<'a> {
    pub cwd: String,
    pub selected_tools: Vec<String>,
    pub tool_snippets: HashMap<String, String>,
    pub prompt_guidelines: Vec<String>,
    pub context_files: Vec<ContextFile>,
    pub skills: Vec<Skill>,
    pub tuning_section: Option<String>,
    pub core_prompt: Option<&'a dyn Fn(&DynamicPromptCoreContext) -> String>,
    pub workstation_dialect: Option<WorkstationDialect>,
}

fn build_context_files_section(context_files: &[ContextFile]) -> String {
    if context_files.is_empty() {
        return String::new();
    }

    let mut lines: Vec<String> = vec![
        "## Project Context".to_string(),
        String::new(),
        "Project instruction files (below, and in [Directory Context: ...] blocks injected during reads) bind files under their directory; deeper files win on conflict; explicit user instructions override.".to_string(),
        String::new(),
    ];
    for context_file in context_files {
        lines.push(format!("### {}", context_file.path));
        lines.push(String::new());
        lines.push(context_file.content.trim_end().to_string());
        lines.push(String::new());
    }
    lines.join("\n").trim_end().to_string()
}

fn current_date() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

/// `buildDynamicSystemPrompt`.
pub fn build_dynamic_system_prompt(options: &BuildDynamicSystemPromptOptions<'_>) -> String {
    let prompt_cwd = options.cwd.replace('\\', "/");
    let tools = categorize_tools(&options.selected_tools);
    let date = current_date();

    let tool_section = build_tool_section(&tools, &options.tool_snippets, &options.prompt_guidelines);

    let mut sections: Vec<String> = match options.core_prompt {
        Some(core_prompt) => vec![core_prompt(&DynamicPromptCoreContext { tools: tools.clone(), tool_section: tool_section.clone() })],
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
            build_style_section(),
        ],
    };

    let tuning = options.tuning_section.as_deref().map(str::trim).filter(|tuning| !tuning.is_empty());
    if let Some(tuning) = tuning {
        sections.push(String::new());
        sections.push(tuning.to_string());
    }

    let context_files_section = build_context_files_section(&options.context_files);
    if !context_files_section.is_empty() {
        sections.push(String::new());
        sections.push(context_files_section);
    }

    let skills_section = format_skills_for_prompt(&options.skills, FileReadTool::Read);
    if !skills_section.is_empty() {
        sections.push(skills_section);
    }

    sections.push(String::new());
    sections.push(build_workstation_section(&BuildWorkstationSectionOptions {
        selected_tools: options.selected_tools.clone(),
        dialect: options.workstation_dialect,
        facts: None,
    }));

    sections.push(String::new());
    sections.push(format!("Current date: {date}"));
    sections.push(format!("Current working directory: {prompt_cwd}"));

    sections.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source_info::{SyntheticSourceInfoOptions, create_synthetic_source_info};
    use crate::skills::Skill;

    fn options() -> BuildDynamicSystemPromptOptions<'static> {
        let mut tool_snippets: HashMap<String, String> = HashMap::new();
        tool_snippets.insert("read".to_string(), "Read file contents".to_string());
        BuildDynamicSystemPromptOptions {
            cwd: "/work".to_string(),
            selected_tools: vec!["read".to_string()],
            tool_snippets,
            prompt_guidelines: Vec::new(),
            context_files: Vec::new(),
            skills: Vec::new(),
            tuning_section: None,
            core_prompt: None,
            workstation_dialect: None,
        }
    }

    fn skill(name: &str) -> Skill {
        let file_path = format!("/skills/{name}/SKILL.md");
        Skill {
            name: name.to_string(),
            description: format!("{name} description"),
            base_dir: format!("/skills/{name}"),
            source_info: create_synthetic_source_info(&file_path, SyntheticSourceInfoOptions { source: "local".to_string(), ..Default::default() }),
            file_path,
            disable_model_invocation: false,
        }
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
        assert!(identity < intent && intent < working && working < verification && verification < tools);
        assert!(tools < policies && policies < style && style < workstation);
        assert!(prompt.ends_with(&format!("Current date: {}\nCurrent working directory: /work", current_date())));
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
        options.context_files = vec![ContextFile { path: "/work/AGENTS.md".to_string(), content: "rules  \n".to_string() }];
        let prompt = build_dynamic_system_prompt(&options);
        assert!(prompt.contains("## Project Context"));
        assert!(prompt.contains("### /work/AGENTS.md\n\nrules\n"));
        assert!(prompt.contains("bind files under their directory; deeper files win on conflict; explicit user instructions override."));
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
        let core = |context: &DynamicPromptCoreContext| format!("CUSTOM CORE\n{}", context.tool_section);
        let mut options = options();
        options.core_prompt = Some(&core);
        options.skills = vec![skill("demo")];
        let prompt = build_dynamic_system_prompt(&options);
        assert!(prompt.starts_with("CUSTOM CORE\n## Available Tools"));
        assert!(!prompt.contains("## Intent Gate"));
        assert!(!prompt.contains("## Style"));
        assert!(prompt.contains("<available_skills>"));
        assert!(prompt.contains("<workstation>"));
        assert!(prompt.contains("Current working directory: /work"));
    }

    #[test]
    fn the_workstation_dialect_is_forwarded() {
        let mut options = options();
        options.workstation_dialect = Some(WorkstationDialect::Claude);
        let prompt = build_dynamic_system_prompt(&options);
        assert!(prompt.contains("<execution_context>"));
    }
}
