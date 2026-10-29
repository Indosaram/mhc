//! The `task` configuration section (config-keys.md, Part B) as typed settings.
use serde_json::Value;
use std::path::PathBuf;
use std::time::Duration;

/// Environment variable carrying a child's nesting depth (0 = the main session).
pub const DEPTH_ENV: &str = "MHC_TASK_DEPTH";
/// Environment variable carrying a child's task id.
pub const TASK_ID_ENV: &str = "MHC_TASK_ID";
/// Environment variable carrying the root session of a delegated run (the top-level session that
/// started the chain of tasks).
pub const ROOT_SESSION_ENV: &str = "MHC_TASK_ROOT_SESSION";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskSettings {
    /// Children running at once per manager (`task.default_concurrency`, 0 = unlimited).
    pub concurrency: usize,
    /// Nesting depth below which a session may spawn (`task.max_depth`; oh-my-pi
    /// `canSpawnAtDepth`).
    pub max_depth: u32,
    pub state_dir: Option<PathBuf>,
    pub isolation_enabled: bool,
    pub wait_min: Duration,
    pub wait_default: Duration,
    pub wait_max: Duration,
}

impl Default for TaskSettings {
    fn default() -> Self {
        Self {
            concurrency: 5,
            max_depth: 1,
            state_dir: None,
            isolation_enabled: false,
            wait_min: Duration::from_millis(5_000),
            wait_default: Duration::from_millis(60_000),
            wait_max: Duration::from_millis(600_000),
        }
    }
}

fn u64_at(value: &Value, path: &[&str]) -> Option<u64> {
    path.iter()
        .try_fold(value, |node, key| node.get(key))?
        .as_u64()
}

impl TaskSettings {
    /// Reads `view.task`; absent or invalid keys keep their defaults.
    pub fn from_view(view: Option<&Value>) -> Self {
        let mut settings = Self::default();
        let Some(task) = view.and_then(|view| view.get("task")) else {
            return settings;
        };
        if let Some(n) = u64_at(task, &["default_concurrency"]) {
            settings.concurrency = usize::try_from(n).unwrap_or(usize::MAX);
        }
        if let Some(n) = u64_at(task, &["max_depth"]) {
            settings.max_depth = u32::try_from(n).unwrap_or(u32::MAX);
        }
        if let Some(dir) = task.get("state_dir").and_then(Value::as_str)
            && !dir.trim().is_empty()
        {
            settings.state_dir = Some(PathBuf::from(dir.trim()));
        }
        settings.isolation_enabled = task
            .get("isolation")
            .and_then(|isolation| isolation.get("enabled"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        for (key, slot) in [
            ("min_ms", &mut settings.wait_min),
            ("default_ms", &mut settings.wait_default),
            ("max_ms", &mut settings.wait_max),
        ] {
            if let Some(ms) = u64_at(task, &["wait", key]).filter(|ms| *ms > 0) {
                *slot = Duration::from_millis(ms);
            }
        }
        settings
    }

    /// `requested` clamped into `[wait_min, wait_max]`; `None` = the default wait.
    pub fn wait_for(&self, requested: Option<Duration>) -> Duration {
        let wait = requested.unwrap_or(self.wait_default);
        wait.clamp(self.wait_min.min(self.wait_max), self.wait_max)
    }
}

/// The current process's nesting depth from [`DEPTH_ENV`].
pub fn current_depth() -> u32 {
    std::env::var(DEPTH_ENV)
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(0)
}

/// The root session of the session `own` running in this process: [`ROOT_SESSION_ENV`] when this
/// process is a delegated run, else `own` itself.
pub fn current_root_session(own: &str) -> String {
    std::env::var(ROOT_SESSION_ENV)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| own.to_owned())
}

/// Whether a session at `depth` may still spawn children (oh-my-pi `canSpawnAtDepth`).
pub fn can_spawn_at_depth(max_depth: u32, depth: u32) -> bool {
    depth < max_depth
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_task_section() {
        let view = json!({"task": {"default_concurrency": 2, "max_depth": 3, "state_dir": "/s",
            "isolation": {"enabled": true}, "wait": {"min_ms": 10, "default_ms": 20, "max_ms": 30}}});
        let settings = TaskSettings::from_view(Some(&view));
        assert_eq!(settings.concurrency, 2);
        assert_eq!(settings.max_depth, 3);
        assert_eq!(settings.state_dir, Some(PathBuf::from("/s")));
        assert!(settings.isolation_enabled);
        assert_eq!(settings.wait_for(None), Duration::from_millis(20));
        assert_eq!(settings.wait_for(Some(Duration::from_secs(9))), Duration::from_millis(30));
        assert_eq!(settings.wait_for(Some(Duration::ZERO)), Duration::from_millis(10));
        assert_eq!(TaskSettings::from_view(None), TaskSettings::default());
    }

    #[test]
    fn depth_gate() {
        assert!(can_spawn_at_depth(1, 0));
        assert!(!can_spawn_at_depth(1, 1));
        assert!(!can_spawn_at_depth(0, 0));
    }
}
