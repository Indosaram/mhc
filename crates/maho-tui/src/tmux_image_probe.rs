//! Port of senpi `packages/tui/src/tmux-image-probe.ts`.

use crate::process_env::Env;
use crate::terminal_capabilities::CellDimensions;
use crate::tmux_cursor_query::{exec_file_sync, TmuxExecFile};

const PROBE_TIMEOUT_MS: u64 = 250;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TmuxSupportTier {
    Unsupported,
    OnOnly,
    OnAndAll,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TmuxAllowPassthrough {
    Off,
    On,
    All,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmuxBaseState {
    pub support_tier: TmuxSupportTier,
    pub allow_passthrough: TmuxAllowPassthrough,
    pub focus_events: bool,
    pub pane_active: bool,
    pub window_active: bool,
    pub visible: bool,
    pub client_count: u32,
    pub client_termname: String,
    pub nested: bool,
    pub hyperlinks: bool,
    pub cell_dimensions: Option<CellDimensions>,
}

impl Default for TmuxBaseState {
    fn default() -> Self {
        Self {
            support_tier: TmuxSupportTier::Unsupported,
            allow_passthrough: TmuxAllowPassthrough::Off,
            focus_events: false,
            pane_active: false,
            window_active: false,
            visible: false,
            client_count: 0,
            client_termname: String::new(),
            nested: false,
            hyperlinks: false,
            cell_dimensions: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TmuxUnavailableReason {
    ProbeFailed,
    MalformedOutput,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TmuxImageState {
    Outside(TmuxBaseState),
    Unavailable {
        base: TmuxBaseState,
        reason: TmuxUnavailableReason,
    },
    Detected {
        base: TmuxBaseState,
        version: String,
    },
}

impl TmuxImageState {
    pub fn base(&self) -> &TmuxBaseState {
        match self {
            TmuxImageState::Outside(base)
            | TmuxImageState::Unavailable { base, .. }
            | TmuxImageState::Detected { base, .. } => base,
        }
    }
}

pub const TMUX_IMAGE_FORMAT: &str =
    "#{version}|#{allow-passthrough}|#{focus-events}|#{pane_active}|#{window_active}|#{session_attached}|#{client_termname}|#{client_cell_width}|#{client_cell_height}|#{client_termfeatures}";

fn disabled_state() -> TmuxBaseState {
    TmuxBaseState::default()
}

fn parse_tmux_version(version: &str) -> Option<(u32, u32)> {
    let trimmed = version.trim();
    let rest = trimmed.strip_prefix("next-").unwrap_or(trimmed);
    let mut parts = rest.splitn(3, '.');
    let major = parts.next()?;
    let minor = parts.next()?;
    if major.is_empty() || minor.is_empty() {
        return None;
    }
    let major_end = major.find(|c: char| !c.is_ascii_digit()).unwrap_or(major.len());
    let minor_end = minor
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(minor.len());
    if major_end == 0 || minor_end == 0 {
        return None;
    }
    let major = major[..major_end].parse::<u32>().ok()?;
    let minor = minor[..minor_end].parse::<u32>().ok()?;
    Some((major, minor))
}

pub fn tmux_support_tier(version: &str) -> TmuxSupportTier {
    let Some((major, minor)) = parse_tmux_version(version) else {
        return TmuxSupportTier::Unsupported;
    };
    if major > 3 || (major == 3 && minor >= 4) {
        return TmuxSupportTier::OnAndAll;
    }
    if major == 3 && minor == 3 {
        return TmuxSupportTier::OnOnly;
    }
    TmuxSupportTier::Unsupported
}

pub fn normalize_tmux_allow_passthrough(
    raw: &str,
    support_tier: TmuxSupportTier,
) -> TmuxAllowPassthrough {
    if support_tier == TmuxSupportTier::Unsupported {
        return TmuxAllowPassthrough::Off;
    }
    let value = raw.trim().to_lowercase();
    if value == "1" || value == "on" {
        return TmuxAllowPassthrough::On;
    }
    if value == "all" && support_tier == TmuxSupportTier::OnAndAll {
        return TmuxAllowPassthrough::All;
    }
    TmuxAllowPassthrough::Off
}

fn parse_boolean(raw: &str) -> bool {
    let value = raw.trim().to_lowercase();
    value == "1" || value == "on"
}

fn parse_positive_integer(raw: &str) -> Option<u32> {
    if raw.is_empty() || !raw.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let value = raw.parse::<u32>().ok()?;
    (value > 0).then_some(value)
}

fn parse_client_count(raw: &str) -> u32 {
    if raw.is_empty() || !raw.bytes().all(|b| b.is_ascii_digit()) {
        return 0;
    }
    raw.parse::<u32>().unwrap_or(0)
}

fn is_nested_client(client_termname: &str) -> bool {
    let term = client_termname.trim().to_lowercase();
    term.starts_with("tmux") || term.starts_with("screen")
}

pub fn parse_tmux_image_state(output: &str) -> TmuxImageState {
    let trimmed = output.trim();
    let fields: Vec<&str> = trimmed.split('|').collect();
    if fields.len() != 10 {
        return TmuxImageState::Unavailable {
            base: disabled_state(),
            reason: TmuxUnavailableReason::MalformedOutput,
        };
    }

    let version = fields[0];
    let raw_allow = fields[1];
    let raw_focus_events = fields[2];
    let raw_pane_active = fields[3];
    let raw_window_active = fields[4];
    let raw_client_count = fields[5];
    let raw_client_termname = fields[6];
    let raw_cell_width = fields[7];
    let raw_cell_height = fields[8];
    let raw_termfeatures = fields[9];

    let support_tier = tmux_support_tier(version);
    let client_count = parse_client_count(raw_client_count);
    let pane_active = parse_boolean(raw_pane_active);
    let window_active = parse_boolean(raw_window_active);
    let client_termname = raw_client_termname.trim().to_string();
    let width_px = parse_positive_integer(raw_cell_width);
    let height_px = parse_positive_integer(raw_cell_height);
    let cell_dimensions = match (width_px, height_px) {
        (Some(width_px), Some(height_px)) => Some(CellDimensions { width_px, height_px }),
        _ => None,
    };

    TmuxImageState::Detected {
        base: TmuxBaseState {
            support_tier,
            allow_passthrough: normalize_tmux_allow_passthrough(raw_allow, support_tier),
            focus_events: parse_boolean(raw_focus_events),
            pane_active,
            window_active,
            visible: pane_active && window_active && client_count == 1,
            client_count,
            client_termname,
            nested: is_nested_client(raw_client_termname),
            hyperlinks: raw_termfeatures
                .split(',')
                .map(str::trim)
                .any(|feature| feature == "hyperlinks"),
            cell_dimensions,
        },
        version: version.trim().to_string(),
    }
}

pub fn probe_tmux_image_state(env: &Env, exec_file: Option<&TmuxExecFile>) -> TmuxImageState {
    let term = env.get("TERM").map(|v| v.to_lowercase()).unwrap_or_default();
    let has_tmux = env.get("TMUX").is_some_and(|value| !value.is_empty());
    if !has_tmux && !term.starts_with("tmux") {
        return TmuxImageState::Outside(disabled_state());
    }

    let pane = env.get("TMUX_PANE");
    let mut args: Vec<String> = vec!["display-message".to_string(), "-p".to_string()];
    if let Some(pane) = pane.filter(|pane| is_tmux_pane_id(pane)) {
        args.push("-t".to_string());
        args.push(pane.clone());
    }
    args.push(TMUX_IMAGE_FORMAT.to_string());

    let output = match exec_file {
        Some(exec_file) => exec_file("tmux", &args),
        None => exec_file_sync("tmux", &args, PROBE_TIMEOUT_MS),
    };

    match output {
        Ok(output) => parse_tmux_image_state(&output),
        Err(_) => TmuxImageState::Unavailable {
            base: disabled_state(),
            reason: TmuxUnavailableReason::ProbeFailed,
        },
    }
}

fn is_tmux_pane_id(pane: &str) -> bool {
    pane.strip_prefix('%')
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
#[path = "tmux_image_probe_tests.rs"]
mod tests;
