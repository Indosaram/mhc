use super::types::TipDefinition;

pub const COMPUTER_TIPS: &[TipDefinition] = &[
    TipDefinition {
        id: "computer.what-it-is",
        bindings: &[],
        requires_command: Some("computer"),
        render: |_keys| "Experimental: your agent can use native apps too: screenshots, windows, clicks and typing. Run /computer to see whether this machine is ready.".to_string(),
    },
    TipDefinition {
        id: "computer.stop-chord",
        bindings: &[],
        requires_command: Some("computer"),
        render: |_keys| format!("Press {} at any moment to stop computer input. It stays stopped until you run /computer resume; the agent cannot resume it.", if cfg!(target_os = "macos") { "Control+Option+Command+Escape" } else { "Ctrl+Alt+Shift+Escape" }),
    },
    TipDefinition {
        id: "computer.background-input",
        bindings: &[],
        requires_command: Some("computer"),
        render: |_keys| "Computer use clicks and types into the target app in the background, so your frontmost app, keyboard focus and cursor stay where you left them.".to_string(),
    },
    TipDefinition {
        id: "computer.look-dont-touch",
        bindings: &[],
        requires_command: Some("computer"),
        render: |_keys| "Want the agent to look but not touch? Start with --permission computer:exec=deny: screenshots and accessibility reads still work, clicks and typing are refused.".to_string(),
    },
    TipDefinition {
        id: "computer.permissions",
        bindings: &[],
        requires_command: Some("computer"),
        render: |_keys| "On macOS, computer use needs Screen Recording and Accessibility for the app you launch from. /computer status shows which one is missing.".to_string(),
    },
];
