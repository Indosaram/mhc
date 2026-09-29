//! Tmux / cmux environment detection.

use crate::cmux_detect::is_cmux_compat_environment;
use crate::env_source::{EnvSource, ProcessEnv, is_set};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SplitDirection {
    /// `-h`: split left/right.
    #[default]
    Horizontal,
    /// `-v`: split top/bottom.
    Vertical,
}

impl SplitDirection {
    #[must_use]
    pub const fn as_flag(self) -> &'static str {
        match self {
            Self::Horizontal => "-h",
            Self::Vertical => "-v",
        }
    }
}

pub fn is_inside_tmux_environment(environment: &dyn EnvSource) -> bool {
    is_set(environment.var("TMUX").as_deref())
}

pub fn is_tmux_pane_compatible_environment(environment: &dyn EnvSource) -> bool {
    is_inside_tmux_environment(environment) || is_cmux_compat_environment(environment)
}

pub fn is_native_tmux_environment(environment: &dyn EnvSource) -> bool {
    is_inside_tmux_environment(environment) && !is_cmux_compat_environment(environment)
}

#[must_use]
pub fn is_inside_tmux() -> bool {
    is_inside_tmux_environment(&ProcessEnv)
}

#[must_use]
pub fn is_native_tmux() -> bool {
    is_native_tmux_environment(&ProcessEnv)
}

#[must_use]
pub fn is_tmux_pane_compatible() -> bool {
    is_tmux_pane_compatible_environment(&ProcessEnv)
}

#[must_use]
pub fn get_current_pane_id() -> Option<String> {
    ProcessEnv.var("TMUX_PANE")
}
