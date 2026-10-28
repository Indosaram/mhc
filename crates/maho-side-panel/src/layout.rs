//! Effective width, column visibility, mount mode and the one-time notice (spec 3).

use crate::settings::Width;
use std::path::{Path, PathBuf};

/// Minimum transcript width kept beside the column.
pub const MIN_TRANSCRIPT: usize = 60;
/// Smallest effective panel width.
pub const MIN_WIDTH: usize = 32;
/// Largest effective panel width.
pub const MAX_WIDTH: usize = 80;

/// Effective panel width for terminal width `terminal` (0 = unknown).
pub fn effective_width(width: Width, terminal: usize) -> usize {
    let raw = match width {
        Width::Columns(n) => n as usize,
        Width::Percent(pct) => terminal * pct as usize / 100,
    };
    let clamped = raw.clamp(MIN_WIDTH, MAX_WIDTH);
    if terminal == 0 {
        return clamped;
    }
    let room = terminal.saturating_sub(MIN_TRANSCRIPT);
    if room < MIN_WIDTH {
        MIN_WIDTH
    } else {
        clamped.min(room)
    }
}

/// Whether the column is visible at `terminal` columns (hidden, not unmounted, below the minimum).
pub fn column_visible(min_columns: u32, terminal: usize) -> bool {
    terminal >= min_columns as usize
}

/// How the panel is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MountMode {
    /// A column beside the transcript (fullscreen layout).
    Column,
    /// A block above the editor (regular layout).
    Block,
}

/// File name of the one-time fullscreen notice marker in the agent home.
pub const NOTICE_MARKER: &str = "side-panel-fullscreen-notice";

/// Path of the notice marker under `agent_dir`.
pub fn notice_marker_path(agent_dir: &Path) -> PathBuf {
    agent_dir.join(NOTICE_MARKER)
}

/// Creates the marker exclusively. `true` only when this call created it, i.e. the notice should
/// be shown now; an existing marker or any creation failure means no notice.
pub fn claim_notice(agent_dir: &Path) -> bool {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(notice_marker_path(agent_dir)).is_ok()
}

/// The informational notice text naming the fullscreen setting/flag.
pub fn fullscreen_notice() -> String {
    "Side panel is shown above the editor. Start with `--tui-mode fullscreen` to show it as a \
     column beside the transcript."
        .into()
}
