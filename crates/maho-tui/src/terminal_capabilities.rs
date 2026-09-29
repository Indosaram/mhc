//! Port of senpi `packages/tui/src/terminal-capabilities.ts`.
//!
//! The tmux branch depends on tmux-image-capability.ts / tmux-image-probe.ts, which plan todo 9
//! ports. Until todo 9 registers its detector through [`set_tmux_image_detector`], the tmux
//! branch reports images off and hyperlinks off (recorded as `partial` in parity.d/6.md).

use std::sync::RwLock;

use crate::process_env::{self, Env};
use crate::tmux_cursor_query::TmuxExecFile;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageProtocol {
    Kitty,
    Iterm2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellDimensions {
    pub width_px: u32,
    pub height_px: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DetectedTerminalCapabilities {
    pub images: Option<ImageProtocol>,
    pub true_color: bool,
    pub hyperlinks: bool,
    pub tmux_passthrough: Option<bool>,
    pub kitty_unicode_placeholders: Option<bool>,
    pub cell_dimensions: Option<CellDimensions>,
}

/// Result of the tmux image probe plus capability decision (todo 9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TmuxImageDetection {
    pub enabled: bool,
    pub placeholder: bool,
    pub hyperlinks: bool,
    pub cell_dimensions: Option<CellDimensions>,
}

pub type TmuxImageDetector = fn(&Env, Option<&TmuxExecFile>) -> TmuxImageDetection;

static TMUX_IMAGE_DETECTOR: RwLock<Option<TmuxImageDetector>> = RwLock::new(None);

pub fn set_tmux_image_detector(detector: TmuxImageDetector) {
    *TMUX_IMAGE_DETECTOR
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(detector);
}

pub fn outer_kitty_graphics_mode(client_termname: &str) -> Option<&'static str> {
    let term = client_termname.trim().to_lowercase();
    (term.contains("kitty") || term.contains("ghostty") || term.contains("warp"))
        .then_some("placeholder")
}

const fn caps(
    images: Option<ImageProtocol>,
    true_color: bool,
    hyperlinks: bool,
) -> DetectedTerminalCapabilities {
    DetectedTerminalCapabilities {
        images,
        true_color,
        hyperlinks,
        tmux_passthrough: None,
        kitty_unicode_placeholders: None,
        cell_dimensions: None,
    }
}

pub fn detect_terminal_capabilities() -> DetectedTerminalCapabilities {
    let env = process_env::current();
    detect_terminal_capabilities_in(&env, crate::native_platform::node_platform(), None)
}

pub fn detect_terminal_capabilities_in(
    env: &Env,
    platform: &str,
    exec_tmux: Option<&TmuxExecFile>,
) -> DetectedTerminalCapabilities {
    let get = |name: &str| env.get(name).map_or("", String::as_str);
    let has = |name: &str| process_env::truthy(env, name);
    let term = get("TERM");
    let term_program = get("TERM_PROGRAM").to_lowercase();
    let terminal_emulator = get("TERMINAL_EMULATOR");
    let color_term = get("COLORTERM");
    let true_color = color_term == "truecolor" || color_term == "24bit";

    if has("TMUX") || term.starts_with("tmux") {
        let detector = *TMUX_IMAGE_DETECTOR
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let decision = detector.map(|d| d(env, exec_tmux)).unwrap_or_default();
        return DetectedTerminalCapabilities {
            images: decision.enabled.then_some(ImageProtocol::Kitty),
            true_color,
            hyperlinks: decision.hyperlinks,
            tmux_passthrough: decision.enabled.then_some(true),
            kitty_unicode_placeholders: (decision.enabled && decision.placeholder).then_some(true),
            cell_dimensions: decision.cell_dimensions,
        };
    }

    if has("KITTY_WINDOW_ID") {
        return caps(Some(ImageProtocol::Kitty), true, true);
    }
    if term_program == "ghostty" {
        return DetectedTerminalCapabilities {
            kitty_unicode_placeholders: Some(true),
            ..caps(Some(ImageProtocol::Kitty), true, true)
        };
    }
    if has("WARP_SESSION_ID") || has("WARP_TERMINAL_SESSION_UUID") || term_program == "warpterminal"
    {
        return caps(Some(ImageProtocol::Kitty), true, true);
    }
    if has("WEZTERM_PANE") || term_program == "wezterm" {
        return caps(Some(ImageProtocol::Kitty), true, true);
    }
    if term_program == "iterm.app" || has("ITERM_SESSION_ID") {
        return caps(Some(ImageProtocol::Iterm2), true, true);
    }
    if term_program == "apple_terminal" {
        return caps(None, false, true);
    }
    if has("WT_SESSION") {
        return caps(None, true, true);
    }
    if term_program == "vscode" {
        return caps(None, true, true);
    }
    if has("CMUX_WORKSPACE_ID") || terminal_emulator.contains("JetBrains") {
        return caps(None, true, false);
    }
    if term.starts_with("screen") {
        return caps(None, true_color, false);
    }
    // Windows Terminal does not always set WT_SESSION; modern Windows consoles support truecolor.
    if platform == "win32" {
        return caps(None, true, false);
    }
    caps(
        (term == "xterm-kitty").then_some(ImageProtocol::Kitty),
        true_color,
        false,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process_env::env_from;

    #[test]
    fn detects_terminal_families() {
        let d = |pairs: &[(&str, &str)]| {
            detect_terminal_capabilities_in(&env_from(pairs), "darwin", None)
        };
        assert_eq!(
            d(&[("KITTY_WINDOW_ID", "1")]).images,
            Some(ImageProtocol::Kitty)
        );
        assert_eq!(
            d(&[("TERM_PROGRAM", "ghostty")]).kitty_unicode_placeholders,
            Some(true)
        );
        assert_eq!(
            d(&[("TERM_PROGRAM", "iTerm.app")]).images,
            Some(ImageProtocol::Iterm2)
        );
        assert_eq!(
            d(&[("TERM_PROGRAM", "Apple_Terminal")]),
            caps(None, false, true)
        );
        assert_eq!(
            d(&[("TERMINAL_EMULATOR", "JetBrains-JediTerm")]),
            caps(None, true, false)
        );
        assert_eq!(
            d(&[("TERM", "screen-256color"), ("COLORTERM", "24bit")]),
            caps(None, true, false)
        );
        assert_eq!(
            d(&[("TERM", "xterm-kitty")]).images,
            Some(ImageProtocol::Kitty)
        );
        assert_eq!(d(&[]), caps(None, false, false));
        let win = detect_terminal_capabilities_in(&env_from(&[]), "win32", None);
        assert_eq!(win, caps(None, true, false));
    }

    #[test]
    fn outer_kitty_graphics_mode_matches_placeholder_terminals() {
        assert_eq!(outer_kitty_graphics_mode(" Ghostty "), Some("placeholder"));
        assert_eq!(outer_kitty_graphics_mode("xterm-256color"), None);
    }
}
