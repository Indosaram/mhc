//! Port of senpi `packages/coding-agent/src/modes/interactive/tips/catalog/workspace-tips.ts`.

use super::types::TipDefinition;

pub const WORKSPACE_TIPS: &[TipDefinition] = &[
    TipDefinition { id: "help-command", bindings: &[], requires_command: Some("help"), render: |_k| "Use /help to see available commands and guidance.".to_string() },
    TipDefinition { id: "keybindings-command", bindings: &[], requires_command: None, render: |_k| "Use /keybindings to review and customize keyboard shortcuts.".to_string() },
    TipDefinition { id: "agents-md-context", bindings: &[], requires_command: None, render: |_k| format!("{} loads AGENTS.md from your home directory, every parent folder, and the current folder as project instructions.", maho_core::config::app_name()) },
    TipDefinition { id: "skills-and-prompts", bindings: &[], requires_command: None, render: |_k| format!("Skills run as /skill:name, and prompt templates in {}/prompts expand as /templatename.", maho_core::config::agent_dir_label()) },
    TipDefinition { id: "files-command", bindings: &[], requires_command: Some("files"), render: |_k| "Use /files to list every file this session read, wrote, or edited.".to_string() },
    TipDefinition { id: "diff-command", bindings: &[], requires_command: Some("diff"), render: |_k| "Use /diff to review this session's git changes and open them in a diff view.".to_string() },
    TipDefinition { id: "todo-command", bindings: &[], requires_command: Some("todo"), render: |_k| "Use /todo to show or edit the todo list without leaving the session.".to_string() },
    TipDefinition { id: "goal-command", bindings: &[], requires_command: Some("goal"), render: |_k| format!("Use /goal to set a persistent goal {} keeps pursuing, then pause, resume, or clear it.", maho_core::config::app_name()) },
    TipDefinition { id: "btw-command", bindings: &[], requires_command: Some("btw"), render: |_k| "Use /btw to ask a side question in a parallel session without disturbing this one.".to_string() },
    TipDefinition { id: "lookat-command", bindings: &[], requires_command: Some("lookat"), render: |_k| "Use /lookat to manage the vision-model chain the look_at tool uses for images and screenshots.".to_string() },
    TipDefinition { id: "mcp-command", bindings: &[], requires_command: Some("mcp"), render: |_k| "Use /mcp to inspect and manage the MCP servers wired into this session.".to_string() },
    TipDefinition { id: "rules-command", bindings: &[], requires_command: Some("rules"), render: |_k| "Use /rules to see which rules are loaded, and /reload-rules to re-read them mid-session.".to_string() },
    TipDefinition { id: "hooks-command", bindings: &[], requires_command: Some("hooks"), render: |_k| "Use /hooks to list loaded hook sources and their diagnostics.".to_string() },
    TipDefinition { id: "websearch-command", bindings: &[], requires_command: Some("websearch"), render: |_k| "Use /websearch to check which web-search provider is currently active.".to_string() },
];
