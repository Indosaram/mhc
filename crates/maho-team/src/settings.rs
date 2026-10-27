//! Pane settings (environment, `docs/cleanroom/specs/team.md` §5) and team limits (the existing
//! `task.team` keys, §6).
use serde_json::Value;
use std::time::Duration;

/// Which delegated tasks get a pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaneMode {
    /// Team members only (default).
    Auto,
    /// Every delegated task.
    All,
    /// None.
    Off,
}

/// Where a pane opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    /// A split of the lead's window.
    Split,
    /// A window of its own.
    Window,
}

/// Terminal multiplexers that can host panes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MuxKind {
    Tmux,
    Cmux,
}

impl MuxKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tmux => "tmux",
            Self::Cmux => "cmux",
        }
    }
}

pub const PANES_ENV: &str = "MAHO_TEAM_PANES";
pub const MUX_ENV: &str = "MAHO_TEAM_MUX";
pub const LAYOUT_ENV: &str = "MAHO_TEAM_PANE_LAYOUT";
pub const LINGER_ENV: &str = "MAHO_TEAM_PANE_LINGER";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneSettings {
    pub mode: PaneMode,
    /// Backend preference (`None` = detect).
    pub preference: Option<MuxKind>,
    pub placement: Placement,
    /// How long a pane stays open after its task settled.
    pub linger: Duration,
}

impl Default for PaneSettings {
    fn default() -> Self {
        Self {
            mode: PaneMode::Auto,
            preference: None,
            placement: Placement::Split,
            linger: Duration::from_secs(3),
        }
    }
}

impl PaneSettings {
    /// Reads the `MAHO_TEAM_*` variables through `env`; unknown values keep the defaults.
    pub fn from_env(env: &dyn Fn(&str) -> Option<String>) -> Self {
        let mut settings = Self::default();
        let get = |key: &str| {
            env(key)
                .map(|v| v.trim().to_ascii_lowercase())
                .filter(|v| !v.is_empty())
        };
        match get(PANES_ENV).as_deref() {
            Some("all") => settings.mode = PaneMode::All,
            Some("off" | "0" | "false" | "none") => settings.mode = PaneMode::Off,
            _ => {}
        }
        settings.preference = match get(MUX_ENV).as_deref() {
            Some("tmux") => Some(MuxKind::Tmux),
            Some("cmux") => Some(MuxKind::Cmux),
            _ => None,
        };
        if get(LAYOUT_ENV).as_deref() == Some("window") {
            settings.placement = Placement::Window;
        }
        if let Some(secs) = get(LINGER_ENV).and_then(|v| v.parse::<u64>().ok()) {
            settings.linger = Duration::from_secs(secs);
        }
        settings
    }

    /// The process environment.
    pub fn from_process_env() -> Self {
        Self::from_env(&|key| std::env::var(key).ok())
    }
}

/// `task.team` limits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TeamLimits {
    pub max_members: usize,
    pub max_parallel: usize,
    pub max_wall_clock: Duration,
}

impl Default for TeamLimits {
    fn default() -> Self {
        Self {
            max_members: 8,
            max_parallel: 4,
            max_wall_clock: Duration::from_secs(120 * 60),
        }
    }
}

impl TeamLimits {
    /// Reads `view.task.team`; absent or out-of-range values keep their defaults.
    pub fn from_view(view: &Value) -> Self {
        let mut limits = Self::default();
        let Some(team) = view.get("task").and_then(|task| task.get("team")) else {
            return limits;
        };
        let int = |key: &str| team.get(key).and_then(Value::as_u64);
        if let Some(n) = int("max_members").filter(|n| (1..=8).contains(n)) {
            limits.max_members = usize::try_from(n).unwrap_or(8);
        }
        if let Some(n) = int("max_parallel_members").filter(|n| (1..=8).contains(n)) {
            limits.max_parallel = usize::try_from(n).unwrap_or(4);
        }
        if let Some(n) = int("max_wall_clock_minutes").filter(|n| *n > 0) {
            limits.max_wall_clock = Duration::from_secs(n.saturating_mul(60));
        }
        limits
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;

    #[test]
    fn env_settings() {
        let env: HashMap<&str, &str> = [
            (PANES_ENV, "ALL"),
            (MUX_ENV, "cmux"),
            (LAYOUT_ENV, "window"),
            (LINGER_ENV, "0"),
        ]
        .into();
        let settings = PaneSettings::from_env(&|k| env.get(k).map(|v| (*v).to_owned()));
        assert_eq!(settings.mode, PaneMode::All);
        assert_eq!(settings.preference, Some(MuxKind::Cmux));
        assert_eq!(settings.placement, Placement::Window);
        assert_eq!(settings.linger, Duration::ZERO);
        let settings = PaneSettings::from_env(&|k| (k == PANES_ENV).then(|| "off".to_owned()));
        assert_eq!(settings.mode, PaneMode::Off);
        assert_eq!(PaneSettings::from_env(&|_| None), PaneSettings::default());
    }

    #[test]
    fn limits_from_view() {
        let limits = TeamLimits::from_view(
            &json!({"task": {"team": {"max_members": 3, "max_parallel_members": 9, "max_wall_clock_minutes": 2}}}),
        );
        assert_eq!(limits.max_members, 3);
        assert_eq!(limits.max_parallel, 4);
        assert_eq!(limits.max_wall_clock, Duration::from_secs(120));
        assert_eq!(TeamLimits::from_view(&json!({})), TeamLimits::default());
    }
}
