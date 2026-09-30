//! Port of senpi `packages/coding-agent/src/core/system-prompt.ts`.

use std::collections::{HashMap, HashSet};

use crate::config::{get_docs_path, get_examples_path, get_readme_path};
use crate::skills::{FileReadTool, Skill, format_skills_for_prompt};

/// A pre-loaded project context file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextFile {
    pub path: String,
    pub content: String,
}

/// `BuildSystemPromptOptions`.
#[derive(Debug, Clone, Default)]
pub struct BuildSystemPromptOptions {
    pub custom_prompt: Option<String>,
    pub selected_tools: Option<Vec<String>>,
    pub tool_snippets: Option<HashMap<String, String>>,
    pub prompt_guidelines: Option<Vec<String>>,
    pub append_system_prompt: Option<String>,
    pub cwd: String,
    pub context_files: Option<Vec<ContextFile>>,
    pub skills: Option<Vec<Skill>>,
}

/// `getEvalOnlyGrepGuideline`: the eval-grep bullet, present when grep is a snippet but not a
/// model-facing tool.
pub fn get_eval_only_grep_guideline(
    selected_tools: &[String],
    tool_snippets: Option<&HashMap<String, String>>,
) -> Option<String> {
    let has_grep_snippet = tool_snippets.and_then(|snippets| snippets.get("grep")).is_some_and(|snippet| !snippet.is_empty());
    if has_grep_snippet && !selected_tools.iter().any(|tool| tool == "grep") {
        Some("Search file contents with tool.grep({ pattern, path }) inside eval; prefer it over rg/grep in shell".to_string())
    } else {
        None
    }
}

fn append_context_files(prompt: &mut String, context_files: &[ContextFile]) {
    if context_files.is_empty() {
        return;
    }
    prompt.push_str("\n\n<project_context>\n\n");
    prompt.push_str("Project-specific instructions and guidelines:\n\n");
    for context_file in context_files {
        prompt.push_str(&format!(
            "<project_instructions path=\"{}\">\n{}\n</project_instructions>\n\n",
            context_file.path, context_file.content
        ));
    }
    prompt.push_str("</project_context>\n");
}

fn skill_file_read_tool(tools: &[String]) -> Option<FileReadTool> {
    ["read", "bash"].iter().find(|tool| tools.iter().any(|selected| selected == *tool)).map(|tool| {
        if *tool == "read" { FileReadTool::Read } else { FileReadTool::Bash }
    })
}

/// `buildSystemPrompt`.
pub fn build_system_prompt(options: &BuildSystemPromptOptions) -> String {
    let prompt_cwd = options.cwd.replace('\\', "/");
    let append_section = match options.append_system_prompt.as_deref() {
        Some(append) if !append.is_empty() => format!("\n\n{append}"),
        _ => String::new(),
    };

    let empty_context: Vec<ContextFile> = Vec::new();
    let empty_skills: Vec<Skill> = Vec::new();
    let context_files = options.context_files.as_ref().unwrap_or(&empty_context);
    let skills = options.skills.as_ref().unwrap_or(&empty_skills);
    let tools: Vec<String> = options
        .selected_tools
        .clone()
        .unwrap_or_else(|| vec!["read".to_string(), "bash".to_string(), "edit".to_string(), "write".to_string()]);
    let read_tool = skill_file_read_tool(&tools);

    if let Some(custom_prompt) = options.custom_prompt.as_deref() {
        let mut prompt = custom_prompt.to_string();
        prompt.push_str(&append_section);
        append_context_files(&mut prompt, context_files);
        if let Some(read_tool) = read_tool
            && !skills.is_empty()
        {
            prompt.push_str(&format_skills_for_prompt(skills, read_tool));
        }
        prompt.push_str(&format!("\nCurrent working directory: {prompt_cwd}\n"));
        return prompt;
    }

    let readme_path = get_readme_path();
    let docs_path = get_docs_path();
    let examples_path = get_examples_path();

    let tool_snippets = options.tool_snippets.as_ref();
    let visible_tools: Vec<&String> = tools
        .iter()
        .filter(|name| tool_snippets.and_then(|snippets| snippets.get(*name)).is_some_and(|snippet| !snippet.is_empty()))
        .collect();
    let tools_list = if visible_tools.is_empty() {
        "(none)".to_string()
    } else {
        visible_tools
            .iter()
            .map(|name| {
                let snippet = tool_snippets.and_then(|snippets| snippets.get(*name)).map(String::as_str).unwrap_or_default();
                format!("- {name}: {snippet}")
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    let mut guidelines_list: Vec<String> = Vec::new();
    let mut guidelines_set: HashSet<String> = HashSet::new();
    let mut add_guideline = |guideline: String| {
        if guidelines_set.insert(guideline.clone()) {
            guidelines_list.push(guideline);
        }
    };

    let has_bash = tools.iter().any(|tool| tool == "bash");
    let has_power_shell = tools.iter().any(|tool| tool == "powershell");
    let has_grep = tools.iter().any(|tool| tool == "grep");
    let has_find = tools.iter().any(|tool| tool == "find");
    let has_ls = tools.iter().any(|tool| tool == "ls");

    match get_eval_only_grep_guideline(&tools, tool_snippets) {
        Some(guideline) => add_guideline(guideline),
        None => {
            if (has_bash || has_power_shell) && !has_grep && !has_find && !has_ls {
                if has_bash && has_power_shell {
                    add_guideline("Use bash or PowerShell for file operations like listing, searching, and finding files".to_string());
                } else if has_power_shell {
                    add_guideline("Use PowerShell for file operations like listing, searching, and finding files".to_string());
                } else {
                    add_guideline("Use bash for file operations like ls, rg, find".to_string());
                }
            }
        }
    }

    for guideline in options.prompt_guidelines.iter().flatten() {
        let normalized = guideline.trim();
        if !normalized.is_empty() {
            add_guideline(normalized.to_string());
        }
    }

    add_guideline("Be concise in your responses".to_string());
    add_guideline("Show file paths clearly when working with files".to_string());

    let guidelines = guidelines_list.iter().map(|guideline| format!("- {guideline}")).collect::<Vec<_>>().join("\n");

    let mut prompt = format!(
        "You are an expert coding assistant operating inside pi, a coding agent harness. You help users by reading files, executing commands, editing code, and writing new files.\n\nAvailable tools:\n{tools_list}\n\nIn addition to the tools above, you may have access to other custom tools depending on the project.\n\nGuidelines:\n{guidelines}\n\nPi documentation (read only when the user asks about pi itself, its SDK, extensions, themes, skills, or TUI):\n- Main documentation: {readme_path}\n- Additional docs: {docs_path}\n- Examples: {examples_path} (extensions, custom tools, SDK)\n- When reading pi docs or examples, resolve docs/... under Additional docs and examples/... under Examples, not the current working directory\n- When asked about: extensions (docs/extensions.md, examples/extensions/), themes (docs/themes.md), skills (docs/skills.md), prompt templates (docs/prompt-templates.md), TUI components (docs/tui.md), keybindings (docs/keybindings.md), SDK integrations (docs/sdk.md), custom providers (docs/custom-provider.md), adding models (docs/models.md), pi packages (docs/packages.md), environment variables (docs/environment-variables.md)\n- When working on pi topics, read the docs and examples, and follow .md cross-references before implementing\n- Always read pi .md files completely and follow links to related docs (e.g., tui.md for TUI API details)"
    );

    prompt.push_str(&append_section);
    append_context_files(&mut prompt, context_files);

    if let Some(read_tool) = read_tool
        && !skills.is_empty()
    {
        prompt.push_str(&format_skills_for_prompt(skills, read_tool));
    }

    prompt.push_str(&format!("\nCurrent working directory: {prompt_cwd}"));

    prompt
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source_info::{SyntheticSourceInfoOptions, create_synthetic_source_info};
    use crate::skills::Skill;

    fn snippets(entries: &[(&str, &str)]) -> HashMap<String, String> {
        entries.iter().map(|(name, snippet)| ((*name).to_string(), (*snippet).to_string())).collect()
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
    fn the_eval_only_grep_guideline_needs_a_snippet_without_a_tool() {
        let grep_snippets = snippets(&[("grep", "Search file contents")]);
        assert!(get_eval_only_grep_guideline(&["read".to_string()], Some(&grep_snippets)).is_some());
        assert!(get_eval_only_grep_guideline(&["read".to_string(), "grep".to_string()], Some(&grep_snippets)).is_none());
        assert!(get_eval_only_grep_guideline(&["read".to_string()], Some(&snippets(&[]))).is_none());
        assert!(get_eval_only_grep_guideline(&["read".to_string()], None).is_none());
    }

    #[test]
    fn the_default_prompt_lists_only_tools_with_a_snippet() {
        let prompt = build_system_prompt(&BuildSystemPromptOptions {
            cwd: "/work".to_string(),
            tool_snippets: Some(snippets(&[("read", "Read file contents"), ("bash", "Run commands")])),
            ..Default::default()
        });
        assert!(prompt.contains("Available tools:\n- read: Read file contents\n- bash: Run commands"));
        assert!(!prompt.contains("- edit:"));
        assert!(prompt.contains("Guidelines:\n- Use bash for file operations like ls, rg, find"));
        assert!(prompt.contains("- Be concise in your responses"));
        assert!(prompt.contains("- Show file paths clearly when working with files"));
        assert!(prompt.ends_with("\nCurrent working directory: /work"));
    }

    #[test]
    fn without_any_snippet_the_tool_list_is_none() {
        let prompt = build_system_prompt(&BuildSystemPromptOptions {
            cwd: "/work".to_string(),
            selected_tools: Some(vec!["read".to_string()]),
            ..Default::default()
        });
        assert!(prompt.contains("Available tools:\n(none)"));
    }

    #[test]
    fn guidelines_are_deduplicated_and_trimmed() {
        let prompt = build_system_prompt(&BuildSystemPromptOptions {
            cwd: "/work".to_string(),
            prompt_guidelines: Some(vec!["  custom rule  ".to_string(), "custom rule".to_string(), "   ".to_string()]),
            ..Default::default()
        });
        assert_eq!(prompt.matches("- custom rule").count(), 1);
    }

    #[test]
    fn the_eval_grep_guideline_replaces_the_bash_file_operations_bullet() {
        let prompt = build_system_prompt(&BuildSystemPromptOptions {
            cwd: "/work".to_string(),
            selected_tools: Some(vec!["bash".to_string()]),
            tool_snippets: Some(snippets(&[("bash", "Run commands"), ("grep", "Search file contents")])),
            ..Default::default()
        });
        assert!(prompt.contains("- Search file contents with tool.grep({ pattern, path }) inside eval; prefer it over rg/grep in shell"));
        assert!(!prompt.contains("- Use bash for file operations"));
    }

    #[test]
    fn a_custom_prompt_replaces_the_default_body() {
        let prompt = build_system_prompt(&BuildSystemPromptOptions {
            custom_prompt: Some("You are custom.".to_string()),
            cwd: "/work".to_string(),
            ..Default::default()
        });
        assert!(prompt.starts_with("You are custom."));
        assert!(!prompt.contains("Available tools:"));
        assert!(prompt.ends_with("\nCurrent working directory: /work\n"));
    }

    #[test]
    fn context_files_and_appends_land_in_both_branches() {
        let context = vec![ContextFile { path: "/work/AGENTS.md".to_string(), content: "rules".to_string() }];
        let prompt = build_system_prompt(&BuildSystemPromptOptions {
            cwd: "/work".to_string(),
            context_files: Some(context.clone()),
            append_system_prompt: Some("EXTRA".to_string()),
            ..Default::default()
        });
        assert!(prompt.contains("\n\nEXTRA\n\n<project_context>\n\n"));
        assert!(prompt.contains("<project_instructions path=\"/work/AGENTS.md\">\nrules\n</project_instructions>"));

        let custom = build_system_prompt(&BuildSystemPromptOptions {
            custom_prompt: Some("custom".to_string()),
            cwd: "/work".to_string(),
            context_files: Some(context),
            ..Default::default()
        });
        assert!(custom.contains("<project_instructions path=\"/work/AGENTS.md\">"));
    }

    #[test]
    fn skills_are_appended_only_when_a_read_tool_is_selected() {
        let with_read = build_system_prompt(&BuildSystemPromptOptions {
            cwd: "/work".to_string(),
            selected_tools: Some(vec!["read".to_string()]),
            skills: Some(vec![skill("demo")]),
            ..Default::default()
        });
        assert!(with_read.contains("<available_skills>"));
        assert!(with_read.contains("<name>demo</name>"));

        let without_read = build_system_prompt(&BuildSystemPromptOptions {
            cwd: "/work".to_string(),
            selected_tools: Some(vec!["edit".to_string()]),
            skills: Some(vec![skill("demo")]),
            ..Default::default()
        });
        assert!(!without_read.contains("<available_skills>"));
    }

    #[test]
    fn a_bash_only_session_loads_skills_through_bash() {
        let prompt = build_system_prompt(&BuildSystemPromptOptions {
            cwd: "/work".to_string(),
            selected_tools: Some(vec!["bash".to_string()]),
            skills: Some(vec![skill("demo")]),
            ..Default::default()
        });
        assert!(prompt.contains("Use bash to load a skill's file"));
    }

    #[test]
    fn windows_style_cwd_separators_are_normalized() {
        let prompt = build_system_prompt(&BuildSystemPromptOptions {
            cwd: "C:\\work\\dir".to_string(),
            custom_prompt: Some("x".to_string()),
            ..Default::default()
        });
        assert!(prompt.ends_with("Current working directory: C:/work/dir\n"));
    }
}
