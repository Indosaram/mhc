//! Port of senpi `packages/coding-agent/src/modes/interactive/tips/catalog/session-tips.ts`.

use super::types::TipDefinition;

pub const SESSION_TIPS: &[TipDefinition] = &[
    TipDefinition { id: "favorite-models-command", bindings: &[], requires_command: None, render: |_k| "Use /favorite-models to choose and reorder the models in your rotation.".to_string() },
    TipDefinition { id: "tree-command", bindings: &[], requires_command: None, render: |_k| "Use /tree to revisit earlier points and switch between session branches.".to_string() },
    TipDefinition { id: "fork-command", bindings: &[], requires_command: None, render: |_k| "Use /fork to create a separate session from an earlier user message.".to_string() },
    TipDefinition { id: "clone-session", bindings: &[], requires_command: None, render: |_k| "Use /clone to duplicate the current branch into its own session before trying something risky.".to_string() },
    TipDefinition { id: "continue-session", bindings: &[], requires_command: None, render: |_k| format!("{} -c continues your most recent session; {} -r opens the session picker.", maho_core::config::app_name(), maho_core::config::app_name()) },
    TipDefinition { id: "session-name", bindings: &[], requires_command: None, render: |_k| "Use /rename [name] to label a session so it is easy to spot in the footer and in /resume.".to_string() },
    TipDefinition { id: "session-info", bindings: &[], requires_command: None, render: |_k| "Use /session to see the session file, id, message count, tokens, and cost.".to_string() },
    TipDefinition { id: "export-share", bindings: &[], requires_command: None, render: |_k| "Use /export to write the session to HTML or JSONL, or /share to upload it as a private gist link.".to_string() },
    TipDefinition { id: "compact-command", bindings: &[], requires_command: None, render: |_k| "Use /compact to compact context now, or /compact <prompt> to tell the summarizer what to keep.".to_string() },
    TipDefinition { id: "auto-compaction", bindings: &[], requires_command: None, render: |_k| "Auto-compaction is on by default - the (auto) marker in the footer shows it; tune it with compaction.* in settings.json.".to_string() },
];
