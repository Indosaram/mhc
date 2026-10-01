//! Prints the dynamic system prompt the Rust port builds for a fixture cwd.
//!
//! The golden harness (tools/golden/system-prompt.mjs) runs this and senpi's buildDynamicSystemPrompt
//! over the same fixture and compares the two byte for byte.
//!
//! Usage: system_prompt_fixture <cwd> <agentDir> <comma-separated tools>

use std::collections::HashMap;

use maho_core::dynamic_prompt::{BuildDynamicSystemPromptOptions, build_dynamic_system_prompt};
use maho_core::resource_loader::load_project_context_files;
use maho_core::skills::{LoadSkillsOptions, load_skills};

fn tool_snippets(tools: &[String]) -> HashMap<String, String> {
    let known: [(&str, &str); 8] = [
        ("read", "Read file contents"),
        ("bash", "Execute bash commands"),
        ("edit", "Make precise file edits"),
        ("write", "Create or overwrite files"),
        ("grep", "Search file contents"),
        ("glob", "Find files by pattern"),
        ("skill", "Load a skill's instructions"),
        ("session_list", "List sessions"),
    ];
    tools
        .iter()
        .filter_map(|tool| {
            known.iter().find(|(name, _)| name == tool).map(|(name, snippet)| ((*name).to_string(), (*snippet).to_string()))
        })
        .collect()
}

fn main() {
    let mut args = std::env::args().skip(1);
    let cwd = args.next().unwrap_or_else(|| ".".to_string());
    let agent_dir = args.next().unwrap_or_else(|| ".".to_string());
    let tools: Vec<String> = args
        .next()
        .unwrap_or_default()
        .split(',')
        .filter(|tool| !tool.is_empty())
        .map(str::to_string)
        .collect();

    let skills = load_skills(&LoadSkillsOptions { cwd: cwd.clone(), agent_dir: agent_dir.clone(), skill_paths: Vec::new(), include_defaults: false });
    let prompt = build_dynamic_system_prompt(&BuildDynamicSystemPromptOptions {
        cwd: cwd.clone(),
        selected_tools: tools.clone(),
        tool_snippets: tool_snippets(&tools),
        prompt_guidelines: Vec::new(),
        context_files: load_project_context_files(&cwd, &agent_dir),
        skills: skills.skills,
        tuning_section: None,
        core_prompt: None,
        workstation_dialect: None,
    });

    print!("{prompt}");
}
