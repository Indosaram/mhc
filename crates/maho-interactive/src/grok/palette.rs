//! Port of senpi `packages/coding-agent/src/modes/interactive/grok/palette.ts`.
//!
//! Typed constants for the grok chrome. The values are measured colour data from the upstream
//! module, not behaviour: the grok-night / grok-day theme JSONs resolve their palette-named keys
//! from these values.

pub type GrokHex = &'static str;

pub mod surfaces {
    use super::GrokHex;
    pub const BASE: GrokHex = "#141414";
    pub const PANEL: GrokHex = "#111111";
    pub const HIGHLIGHT: GrokHex = "#242424";
    pub const ALT_ROW: GrokHex = "#1c1c1c";
    pub const SELECTED: GrokHex = "#363636";
}

pub mod text {
    use super::GrokHex;
    pub const PRIMARY: GrokHex = "#e1e1e1";
    pub const SECONDARY: GrokHex = "#c8c8c8";
    pub const MUTED: GrokHex = "#6c6c6c";
    pub const DIM: GrokHex = "#585858";
    pub const FAINT: GrokHex = "#505058";
    pub const LABEL: GrokHex = "#808080";
}

pub mod accents {
    use super::GrokHex;
    pub const GREEN: GrokHex = "#9ece6a";
    pub const RED: GrokHex = "#f7768e";
    pub const BLUE: GrokHex = "#7aa2f7";
    pub const YELLOW: GrokHex = "#e0af68";
    pub const CYAN: GrokHex = "#3a95ab";
}

pub mod borders {
    use super::GrokHex;
    pub const INPUT: GrokHex = "#505058";
    pub const CARD: GrokHex = "#333333";
    pub const MODAL: GrokHex = "#585858";
}

pub mod grok_night {
    use super::GrokHex;
    pub const MAGENTA: GrokHex = "#bb9af7";
    pub const BLUE: GrokHex = "#7aa2f7";
    pub const CYAN: GrokHex = "#73daca";
    pub const FG: GrokHex = "#c0caf5";
    pub const GREEN: GrokHex = "#9ece6a";
    pub const RED: GrokHex = "#f7768e";
}

pub mod grok_day {
    use super::GrokHex;
    pub const BLUE: GrokHex = "#2F64D2";
    pub const RED: GrokHex = "#CD3048";
    pub const GREEN: GrokHex = "#0C947C";
}

pub mod glyphs {
    pub const SPINNER: &str = "⠹";
    pub const TOOL_ROW: &str = "┃ ◆";
    pub const TOOL_ROW_GUIDE: &str = "┃";
    pub const TOOL_ROW_MARKER: &str = "◆";
}

pub const REFERENCE_SIZES: &[(usize, usize)] = &[(120, 36), (80, 24)];
