//! Port of senpi `packages/coding-agent/src/modes/interactive/tips/catalog/input-tips.ts`.

use super::types::TipDefinition;

pub const INPUT_TIPS: &[TipDefinition] = &[
    TipDefinition {
        id: "click-pending-question",
        bindings: &[],
        requires_command: None,
        render: |_keys| "Click a pending question's option to answer. Use the displayed selection-bypass modifier to select terminal text, or disable terminal.mouse in /settings.".to_string(),
    },
    TipDefinition {
        id: "queue-follow-up",
        bindings: &["app.message.followUp"],
        requires_command: None,
        render: |keys| format!("While the agent is working, use {} to queue a follow-up for after it finishes.", keys("app.message.followUp")),
    },
    TipDefinition {
        id: "steering-message",
        bindings: &["tui.input.submit", "app.message.followUp"],
        requires_command: None,
        render: |keys| {
            format!(
                "Press {} while the agent works to steer it after the current tool batch; {} waits until all work is done.",
                keys("tui.input.submit"),
                keys("app.message.followUp")
            )
        },
    },
    TipDefinition {
        id: "edit-queued-message",
        bindings: &["app.message.dequeue"],
        requires_command: None,
        render: |keys| format!("Use {} to bring queued messages back for editing.", keys("app.message.dequeue")),
    },
    TipDefinition {
        id: "interrupt-restores-queue",
        bindings: &["app.interrupt"],
        requires_command: None,
        render: |keys| format!("Press {} to abort the current turn and restore queued messages to the editor.", keys("app.interrupt")),
    },
    TipDefinition {
        id: "file-reference",
        bindings: &[],
        requires_command: None,
        render: |_keys| "Type @ in the editor to fuzzy-search project files and insert their paths.".to_string(),
    },
    TipDefinition {
        id: "path-completion",
        bindings: &["tui.input.tab"],
        requires_command: None,
        render: |keys| format!("Press {} to complete file paths while typing.", keys("tui.input.tab")),
    },
    TipDefinition {
        id: "shortcut-overlay",
        bindings: &[],
        requires_command: None,
        render: |_keys| "Type ? in an empty editor to pop up the full shortcut grid.".to_string(),
    },
    TipDefinition {
        id: "prompt-history",
        bindings: &["app.history.search"],
        requires_command: Some("history"),
        render: |keys| format!("Search prompt history across sessions with {}.", keys("app.history.search")),
    },
    TipDefinition {
        id: "external-editor",
        bindings: &["app.editor.external"],
        requires_command: None,
        render: |keys| format!("Open the current prompt in your external editor with {}.", keys("app.editor.external")),
    },
    TipDefinition {
        id: "expand-tool-output",
        bindings: &["app.tools.expand"],
        requires_command: None,
        render: |keys| format!("Collapse or expand tool output with {}.", keys("app.tools.expand")),
    },
    TipDefinition {
        id: "thinking-blocks",
        bindings: &["app.thinking.toggle"],
        requires_command: None,
        render: |keys| format!("Collapse or expand thinking blocks with {}.", keys("app.thinking.toggle")),
    },
    TipDefinition {
        id: "paste-image",
        bindings: &["app.clipboard.pasteImage"],
        requires_command: None,
        render: |keys| format!("Paste an image from the clipboard with {}.", keys("app.clipboard.pasteImage")),
    },
    TipDefinition {
        id: "copy-message",
        bindings: &["app.message.copy"],
        requires_command: None,
        render: |keys| format!("Copy the latest assistant message with {}.", keys("app.message.copy")),
    },
    TipDefinition {
        id: "input-newline",
        bindings: &["tui.input.newLine"],
        requires_command: None,
        render: |keys| format!("Insert a newline without sending with {}.", keys("tui.input.newLine")),
    },
    TipDefinition {
        id: "bash-prefixes",
        bindings: &[],
        requires_command: None,
        render: |_keys| "Prefix a prompt with ! to run bash, or !! to run bash without adding its output to model context.".to_string(),
    },
    TipDefinition {
        id: "drag-drop-files",
        bindings: &[],
        requires_command: None,
        render: |_keys| "Drag and drop files into the terminal to add their paths to your prompt.".to_string(),
    },
    TipDefinition {
        id: "answer-agent-question",
        bindings: &[],
        requires_command: None,
        render: |_keys| "When the agent asks you a question, pick options with digits or arrows, press Enter to advance, and use the final Submit tab for a comment or partial answers; anything unanswered is reported back as unanswered.".to_string(),
    },
    TipDefinition {
        id: "open-pending-question",
        bindings: &["app.question.answer", "app.question.next"],
        requires_command: None,
        render: |keys| {
            format!(
                "Pending questions queue above the editor. An empty-composer digit selects an option; {} opens the shown request and {} cycles requests. /answer lists them.",
                keys("app.question.answer"),
                keys("app.question.next")
            )
        },
    },
];
