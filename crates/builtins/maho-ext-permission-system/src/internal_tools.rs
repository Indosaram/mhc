//! Port of `permission-system/internal-tools.ts` (senpi 903b8336).

/// Harness state and observation tools, not general code or filesystem access.
pub const INTERNAL_PERMISSION_TOOLS: &[&str] = &[
    "todo",
    "tool_search",
    "ask_user",
    "request_user_input",
    "ask_user_question",
    "memory",
    "monitor",
    "create_goal",
    "update_goal",
    "get_goal",
    "bash_output",
    "bash_resize",
    "kill_bash",
];

pub fn is_internal_permission_tool(tool_name: &str) -> bool {
    INTERNAL_PERMISSION_TOOLS.contains(&tool_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_tools_are_exactly_the_upstream_thirteen() {
        let mut names = INTERNAL_PERMISSION_TOOLS.to_vec();
        names.sort_unstable();
        assert_eq!(
            names,
            [
                "ask_user",
                "ask_user_question",
                "bash_output",
                "bash_resize",
                "create_goal",
                "get_goal",
                "kill_bash",
                "memory",
                "monitor",
                "request_user_input",
                "todo",
                "tool_search",
                "update_goal",
            ]
        );
    }

    #[test]
    fn membership_covers_harness_tools_and_excludes_code_and_filesystem_tools() {
        for name in INTERNAL_PERMISSION_TOOLS {
            assert!(is_internal_permission_tool(name), "{name}");
        }
        for name in [
            "bash",
            "eval",
            "read",
            "write",
            "edit",
            "apply_patch",
            "grep",
            "ls",
            "Todo",
            "todo ",
            "",
        ] {
            assert!(!is_internal_permission_tool(name), "{name:?}");
        }
    }
}
