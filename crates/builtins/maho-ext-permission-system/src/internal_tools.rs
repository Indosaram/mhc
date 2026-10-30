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
