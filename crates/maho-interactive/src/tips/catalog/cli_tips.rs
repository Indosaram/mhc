//! Port of senpi `packages/coding-agent/src/modes/interactive/tips/catalog/cli-tips.ts`.

use super::types::TipDefinition;

pub const CLI_TIPS: &[TipDefinition] = &[
    TipDefinition {
        id: "print-mode",
        bindings: &[],
        requires_command: None,
        render: |_keys| {
            format!(
                "{} -p \"question\" answers without the TUI, and piped stdin is merged into the prompt.",
                maho_core::config::app_name()
            )
        },
    },
    TipDefinition {
        id: "tool-flags",
        bindings: &[],
        requires_command: None,
        render: |_keys| "Limit a run's tools with -t read,bash, or drop a few with -xt write,edit.".to_string(),
    },
];
