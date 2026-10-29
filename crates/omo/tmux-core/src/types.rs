//! Tmux configuration and spawn result types.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TmuxLayout {
    MainHorizontal,
    MainVertical,
    Tiled,
    EvenHorizontal,
    EvenVertical,
}

impl TmuxLayout {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MainHorizontal => "main-horizontal",
            Self::MainVertical => "main-vertical",
            Self::Tiled => "tiled",
            Self::EvenHorizontal => "even-horizontal",
            Self::EvenVertical => "even-vertical",
        }
    }
}

pub const TMUX_LAYOUT_VALUES: [TmuxLayout; 5] = [
    TmuxLayout::MainHorizontal,
    TmuxLayout::MainVertical,
    TmuxLayout::Tiled,
    TmuxLayout::EvenHorizontal,
    TmuxLayout::EvenVertical,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TmuxIsolation {
    Inline,
    Window,
    Session,
}

impl TmuxIsolation {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Inline => "inline",
            Self::Window => "window",
            Self::Session => "session",
        }
    }
}

pub const TMUX_ISOLATION_VALUES: [TmuxIsolation; 3] = [
    TmuxIsolation::Inline,
    TmuxIsolation::Window,
    TmuxIsolation::Session,
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TmuxConfig {
    pub enabled: bool,
    pub layout: TmuxLayout,
    pub main_pane_size: u32,
    pub main_pane_min_width: u32,
    pub agent_pane_min_width: u32,
    pub isolation: TmuxIsolation,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpawnPaneResult {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
}

impl SpawnPaneResult {
    #[must_use]
    pub const fn failure() -> Self {
        Self {
            success: false,
            pane_id: None,
        }
    }

    #[must_use]
    pub const fn spawned(pane_id: String) -> Self {
        Self {
            success: true,
            pane_id: Some(pane_id),
        }
    }
}
