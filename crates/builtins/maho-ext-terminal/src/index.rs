pub use crate::extension::TerminalExtension;
pub use crate::settings::{
    TERMINAL_SETTINGS_DEFAULTS, load_terminal_settings, resolve_terminal_settings,
};
pub use crate::shared::{
    TERMINAL_BASH_TOOL, TERMINAL_COMPANION_TOOLS, TERMINAL_INPUT_TOOL, TERMINAL_KILL_TOOL,
    TERMINAL_MONITOR_TOOL, TERMINAL_OUTPUT_TOOL, TERMINAL_RESIZE_TOOL,
};

#[cfg(test)]
mod tests {
    /// index.ts: the settings loaders and every tool name are reachable through the index surface.
    #[test]
    fn index_reexports_settings_loaders_and_tool_names() {
        let global = serde_json::json!({"defaultCols": 90, "maxSessions": 4});
        let project = serde_json::json!({"defaultCols": 100});
        let loaded = super::load_terminal_settings(Some(&global), Some(&project));
        assert_eq!(loaded.default_cols, 100.0);
        assert_eq!(loaded.max_sessions, 4.0);
        assert_eq!(
            super::resolve_terminal_settings(None),
            super::TERMINAL_SETTINGS_DEFAULTS
        );
        assert_eq!(
            [
                super::TERMINAL_BASH_TOOL,
                super::TERMINAL_OUTPUT_TOOL,
                super::TERMINAL_KILL_TOOL,
                super::TERMINAL_INPUT_TOOL,
                super::TERMINAL_RESIZE_TOOL,
                super::TERMINAL_MONITOR_TOOL,
            ],
            [
                "bash",
                "bash_output",
                "kill_bash",
                "bash_input",
                "bash_resize",
                "monitor"
            ]
        );
        assert_eq!(super::TERMINAL_COMPANION_TOOLS.len(), 5);
        let _: super::TerminalExtension = super::TerminalExtension;
    }
}
