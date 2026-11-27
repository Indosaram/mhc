//! Port of senpi `packages/coding-agent/src/core/dynamic-prompt/types.ts`.

/// `AvailableTool["category"]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCategory {
    Search,
    Session,
    Command,
    Other,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PromptSurface {
    #[default]
    Terminal,
    App,
    Chat,
}

/// Which wording the shared prompt generators render (senpi#5665): `Full` is their text from
/// before the senpi#5335 diet, `Diet` (default) the slimmed text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SharedSections {
    Full,
    #[default]
    Diet,
}

/// `AvailableTool`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvailableTool {
    pub name: String,
    pub category: ToolCategory,
}
