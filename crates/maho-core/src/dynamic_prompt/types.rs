//! Port of senpi `packages/coding-agent/src/core/dynamic-prompt/types.ts`.

/// `AvailableTool["category"]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCategory {
    Search,
    Session,
    Command,
    Other,
}

/// `AvailableTool`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvailableTool {
    pub name: String,
    pub category: ToolCategory,
}
