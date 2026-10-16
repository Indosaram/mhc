//! Port of senpi `packages/coding-agent/src/modes/interactive/grok/palette.ts`.
//!
//! Glyphs for grok chrome. Color ownership belongs to theme JSON files.

pub mod glyphs {
    pub const SPINNER: &str = "⠹";
    pub const TOOL_ROW: &str = "┃ ◆";
    pub const TOOL_ROW_GUIDE: &str = "┃";
    pub const TOOL_ROW_MARKER: &str = "◆";
}

pub const REFERENCE_SIZES: &[(usize, usize)] = &[(120, 36), (80, 24)];
