pub const SENPI_ULTRAWORK_DIRECTIVE: &str = include_str!("directive.txt");
pub const SENPI_ASTRA_ULTRAWORK_DIRECTIVE: &str = include_str!(concat!(env!("OUT_DIR"), "/astra-directive.txt"));
pub const FORBIDDEN_DIRECTIVE_TOKENS: [&str; 8] = ["multi_agent", "spawn_agent", "update_plan", "wait_agent", "fork_context", "fork_turns", "codex", "wait_for"];
