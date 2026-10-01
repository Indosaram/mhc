//! Port of senpi packages/coding-agent/src/core/terminal-settings.ts.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TerminalMouseMode {
    Off,
    WhilePending,
    Always,
}

impl TerminalMouseMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "off" => Some(Self::Off),
            "whilePending" => Some(Self::WhilePending),
            "always" => Some(Self::Always),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::WhilePending => "whilePending",
            Self::Always => "always",
        }
    }
}

pub const TERMINAL_MOUSE_MODES: [TerminalMouseMode; 3] =
    [TerminalMouseMode::Off, TerminalMouseMode::WhilePending, TerminalMouseMode::Always];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ImageProtocol {
    Kitty,
    Iterm2,
    Auto,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TimeoutAction {
    Background,
    Kill,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TerminalNotify {
    Wake,
    NextTurn,
    Off,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TerminalSettings {
    pub mouse: Option<TerminalMouseMode>,
    pub show_images: Option<bool>,
    pub image_width_cells: Option<i64>,
    pub clear_on_shrink: Option<bool>,
    pub show_terminal_progress: Option<bool>,
    pub hyperlinks: Option<serde_json::Value>,
    pub images: Option<serde_json::Value>,
    pub true_color: Option<serde_json::Value>,
    pub default_cols: Option<i64>,
    pub default_rows: Option<i64>,
    pub scrollback: Option<i64>,
    pub max_sessions: Option<i64>,
    pub timeout_action: Option<TimeoutAction>,
    pub notify: Option<TerminalNotify>,
    pub monitor_coalesce_window_ms: Option<i64>,
    pub monitor_rate_limit_ms: Option<i64>,
    pub monitor_max_lines_per_injection: Option<i64>,
    pub monitor_max_chars_per_injection: Option<i64>,
    pub monitor_wake_budget: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BranchSummarySettings {
    pub reserve_tokens: Option<i64>,
    pub skip_prompt: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mouse_modes_round_trip() {
        assert_eq!(TerminalMouseMode::parse("whilePending"), Some(TerminalMouseMode::WhilePending));
        assert_eq!(TerminalMouseMode::parse("always").map(TerminalMouseMode::as_str), Some("always"));
        assert_eq!(TerminalMouseMode::parse("nope"), None);
        assert_eq!(TERMINAL_MOUSE_MODES.len(), 3);
    }

    #[test]
    fn terminal_settings_deserialize_camel_case() {
        let settings: TerminalSettings =
            serde_json::from_str(r#"{"mouse":"off","showImages":true,"imageWidthCells":80,"defaultCols":120,"timeoutAction":"kill","notify":"off"}"#)
                .expect("settings");
        assert_eq!(settings.mouse, Some(TerminalMouseMode::Off));
        assert_eq!(settings.show_images, Some(true));
        assert_eq!(settings.image_width_cells, Some(80));
        assert_eq!(settings.timeout_action, Some(TimeoutAction::Kill));
        assert_eq!(settings.notify, Some(TerminalNotify::Off));
    }

    #[test]
    fn empty_settings_use_defaults() {
        let settings: TerminalSettings = serde_json::from_str("{}").expect("settings");
        assert_eq!(settings, TerminalSettings::default());
        let branch: BranchSummarySettings = serde_json::from_str("{}").expect("branch");
        assert_eq!(branch, BranchSummarySettings::default());
    }
}
