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

/// Which image protocol the TUI must probe for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageProbe {
    Kitty,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DetectedTerminalCapabilities {
    pub images: Option<ImageProtocol>,
    pub true_color: bool,
    pub hyperlinks: bool,
    pub tmux_passthrough: Option<bool>,
    pub kitty_unicode_placeholders: Option<bool>,
    pub cell_dimensions: Option<CellDimensions>,
    /// Set only while image support is unknown from the environment: the TUI must query the
    /// terminal and record the answer via `set_probed_image_protocol`.
    pub image_probe: Option<ImageProbe>,
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

/// VS Code's integrated terminal normally sets `TERM_PROGRAM=vscode`, but a terminal reached through
/// WSL (or a shell profile that resets it) can arrive with `TERM_PROGRAM` empty (#2826). VS Code sets
/// `VSCODE_IPC_HOOK_CLI` in every integrated terminal, local or remote. Every process started from that
/// terminal inherits it too, so it counts only when `TERM_PROGRAM` is empty: a terminal that sets
/// `TERM_PROGRAM` has claimed the session. Both detectors check this after every other terminal.
pub fn is_vs_code_terminal(env: &Env) -> bool {
    let term_program = env
        .get("TERM_PROGRAM")
        .map(|v| v.to_lowercase())
        .unwrap_or_default();
    term_program == "vscode"
        || (term_program.is_empty() && process_env::truthy(env, "VSCODE_IPC_HOOK_CLI"))
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
        image_probe: None,
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
    let true_color =
        color_term == "truecolor" || color_term == "24bit" || term.ends_with("-direct");

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
            image_probe: None,
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
    if term.starts_with("screen") {
        return caps(None, true_color, false);
    }
    if has("CMUX_WORKSPACE_ID") || terminal_emulator.contains("JetBrains") {
        return caps(None, true, false);
    }
    if is_vs_code_terminal(env) {
        return caps(None, true, true);
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
        assert!(d(&[("TERM", "xterm-truecolor-direct")]).true_color);
        assert!(!d(&[("TERM", "xterm-256color")]).true_color);
    }

    const REMOTE_WSL_VS_CODE: [(&str, &str); 3] = [
        ("TERM", "xterm-256color"),
        ("VSCODE_IPC_HOOK_CLI", "/run/user/1000/vscode-ipc-1.sock"),
        ("VSCODE_GIT_IPC_HANDLE", "/run/user/1000/vscode-git-1.sock"),
    ];

    fn with_extra(extra: &[(&'static str, &'static str)]) -> Env {
        let mut pairs: Vec<(&str, &str)> = REMOTE_WSL_VS_CODE.to_vec();
        for (key, value) in extra {
            pairs.retain(|(k, _)| k != key);
            pairs.push((key, value));
        }
        env_from(&pairs)
    }

    #[test]
    fn vs_code_without_term_program_turns_hyperlinks_on() {
        let env = env_from(&REMOTE_WSL_VS_CODE);
        assert!(is_vs_code_terminal(&env));
        assert!(detect_terminal_capabilities_in(&env, "linux", None).hyperlinks);
        let git_only = env_from(&[
            ("TERM", "xterm-256color"),
            ("VSCODE_GIT_IPC_HANDLE", "/run/user/1000/vscode-git-1.sock"),
        ]);
        assert!(!is_vs_code_terminal(&git_only));
        let plain = env_from(&[("TERM", "xterm-256color")]);
        assert!(!detect_terminal_capabilities_in(&plain, "linux", None).hyperlinks);
    }

    #[test]
    fn a_more_specific_terminal_wins_over_the_inherited_vs_code_marker() {
        let other = with_extra(&[("TERM_PROGRAM", "someterm")]);
        assert!(!detect_terminal_capabilities_in(&other, "linux", None).hyperlinks);
        for env in [
            with_extra(&[("TERM", "screen-256color")]),
            with_extra(&[("TERM", "screen-256color"), ("TERM_PROGRAM", "vscode")]),
        ] {
            assert!(!detect_terminal_capabilities_in(&env, "linux", None).hyperlinks);
        }
        let jetbrains = with_extra(&[("TERMINAL_EMULATOR", "JetBrains-JediTerm")]);
        assert!(!detect_terminal_capabilities_in(&jetbrains, "linux", None).hyperlinks);
    }

    #[test]
    fn outer_kitty_graphics_mode_matches_placeholder_terminals() {
        assert_eq!(outer_kitty_graphics_mode(" Ghostty "), Some("placeholder"));
        assert_eq!(outer_kitty_graphics_mode("xterm-256color"), None);
    }
}
