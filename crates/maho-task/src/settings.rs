//! The `task` configuration section (config-keys.md, Part B) as typed settings.
use serde_json::Value;
use std::path::PathBuf;
use std::time::Duration;

pub use crate::transport::{
    DEFAULT_COMPLETION_BATCH_LIMIT, DEFAULT_COMPLETION_TRANSPORT_LIMIT,
    MAX_COMPLETION_BATCH_LIMIT, MAX_COMPLETION_TRANSPORT_LIMIT,
    MIN_COMPLETION_BATCH_LIMIT, MIN_COMPLETION_TRANSPORT_LIMIT,
};

/// Environment variable carrying a child's nesting depth (0 = the main session).
pub const DEPTH_ENV: &str = "MHC_TASK_DEPTH";
/// Environment variable carrying a child's task id.
pub const TASK_ID_ENV: &str = "MHC_TASK_ID";
/// Environment variable carrying the root session of a delegated run (the top-level session that
/// started the chain of tasks).
pub const ROOT_SESSION_ENV: &str = "MHC_TASK_ROOT_SESSION";
/// Environment variable carrying the session that spawned a delegated run (its parent; task.md
/// Delta D1).
pub const PARENT_SESSION_ENV: &str = "MAHO_TASK_PARENT_SESSION";

/// The ancestry a delegated run records in a new session header (`spawnedBy`, task.md Delta D1).
/// Serializes with the header's field names.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpawnedBy {
    pub parent_session_id: String,
    pub root_session_id: String,
    pub task_id: String,
}

/// The `spawnedBy` ancestry of this process from `env`: present only when [`TASK_ID_ENV`] and
/// [`PARENT_SESSION_ENV`] are both non-empty. The root is [`ROOT_SESSION_ENV`], or the parent when
/// that is empty. Callers use it only when they create a **new** session file.
pub fn spawned_by_from(env: &dyn Fn(&str) -> Option<String>) -> Option<SpawnedBy> {
    let read = |key: &str| {
        env(key)
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
    };
    let task_id = read(TASK_ID_ENV)?;
    let parent = read(PARENT_SESSION_ENV)?;
    let root = read(ROOT_SESSION_ENV).unwrap_or_else(|| parent.clone());
    Some(SpawnedBy {
        parent_session_id: parent,
        root_session_id: root,
        task_id,
    })
}

/// [`spawned_by_from`] over the process environment.
pub fn current_spawned_by() -> Option<SpawnedBy> {
    spawned_by_from(&|key: &str| std::env::var(key).ok())
}

/// Where the process that owns a task manager sits in the session tree.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Lineage {
    /// Nesting depth of this process ([`DEPTH_ENV`]).
    pub depth: u32,
    /// Root session of this process when it is a delegated run ([`ROOT_SESSION_ENV`]).
    pub root_session: Option<String>,
}

impl Lineage {
    /// The lineage of this process from its environment.
    pub fn current() -> Self {
        Self {
            depth: current_depth(),
            root_session: std::env::var(ROOT_SESSION_ENV)
                .ok()
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty()),
        }
    }

    /// The root session of the session `own` running in this process.
    pub fn root_of(&self, own: &str) -> String {
        self.root_session.clone().unwrap_or_else(|| own.to_owned())
    }
}

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
    /// Per-child completion response limit in UTF-16 code units (§9.1).
    pub completion_transport_limit: usize,
    /// Parent wake batch limit in UTF-16 code units (§9.2).
    pub completion_batch_limit: usize,
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
            completion_transport_limit: DEFAULT_COMPLETION_TRANSPORT_LIMIT,
            completion_batch_limit: DEFAULT_COMPLETION_BATCH_LIMIT,
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
        if let Some(n) = u64_at(task, &["completion_transport_limit"])
            .or_else(|| u64_at(task, &["completionTransportLimit"]))
        {
            let val = usize::try_from(n).unwrap_or(usize::MAX);
            settings.completion_transport_limit = val.clamp(
                MIN_COMPLETION_TRANSPORT_LIMIT,
                MAX_COMPLETION_TRANSPORT_LIMIT,
            );
        }
        if let Some(n) = u64_at(task, &["completion_batch_limit"])
            .or_else(|| u64_at(task, &["completionBatchLimit"]))
        {
            let val = usize::try_from(n).unwrap_or(usize::MAX);
            settings.completion_batch_limit = val.clamp(
                MIN_COMPLETION_BATCH_LIMIT,
                MAX_COMPLETION_BATCH_LIMIT,
            );
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
    fn reads_and_clamps_transport_limits() {
        let custom = json!({"task": {"completion_transport_limit": 5000, "completion_batch_limit": 30000}});
        let s = TaskSettings::from_view(Some(&custom));
        assert_eq!(s.completion_transport_limit, 5000);
        assert_eq!(s.completion_batch_limit, 30000);

        let low = json!({"task": {"completion_transport_limit": 500, "completion_batch_limit": 100}});
        let s = TaskSettings::from_view(Some(&low));
        assert_eq!(s.completion_transport_limit, MIN_COMPLETION_TRANSPORT_LIMIT);
        assert_eq!(s.completion_batch_limit, MIN_COMPLETION_BATCH_LIMIT);

        let high = json!({"task": {"completion_transport_limit": 100_000, "completion_batch_limit": 200_000}});
        let s = TaskSettings::from_view(Some(&high));
        assert_eq!(s.completion_transport_limit, MAX_COMPLETION_TRANSPORT_LIMIT);
        assert_eq!(s.completion_batch_limit, MAX_COMPLETION_BATCH_LIMIT);
    }

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |key: &str| pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
    }

    /// task.md Delta D1 tests 1, 3 and 4 (the header values a child derives from its env).
    #[test]
    fn spawned_by_from_the_environment() {
        // Top-level parent P starts T: the root is P.
        let top = env_of(&[(TASK_ID_ENV, "T"), (PARENT_SESSION_ENV, "P"), (ROOT_SESSION_ENV, "P")]);
        assert_eq!(
            spawned_by_from(&top),
            Some(SpawnedBy {
                parent_session_id: "P".into(),
                root_session_id: "P".into(),
                task_id: "T".into()
            })
        );
        let json = serde_json::to_value(spawned_by_from(&top).expect("ancestry")).expect("json");
        assert_eq!(json, json!({"parentSessionId": "P", "rootSessionId": "P", "taskId": "T"}));
        // Child C (root P) starts T2.
        let nested = env_of(&[(TASK_ID_ENV, "T2"), (PARENT_SESSION_ENV, "C"), (ROOT_SESSION_ENV, "P")]);
        let ancestry = spawned_by_from(&nested).expect("ancestry");
        assert_eq!(
            (ancestry.parent_session_id.as_str(), ancestry.root_session_id.as_str(), ancestry.task_id.as_str()),
            ("C", "P", "T2")
        );
        // An empty root falls back to the parent.
        let no_root = env_of(&[(TASK_ID_ENV, "T"), (PARENT_SESSION_ENV, "P"), (ROOT_SESSION_ENV, " ")]);
        assert_eq!(spawned_by_from(&no_root).expect("ancestry").root_session_id, "P");
        // Missing or empty parent: no ancestry.
        assert_eq!(spawned_by_from(&env_of(&[(TASK_ID_ENV, "T"), (PARENT_SESSION_ENV, "")])), None);
        assert_eq!(spawned_by_from(&env_of(&[(TASK_ID_ENV, "T")])), None);
        assert_eq!(spawned_by_from(&env_of(&[(PARENT_SESSION_ENV, "P")])), None);
    }

    #[test]
    fn lineage_root() {
        assert_eq!(Lineage::default().root_of("L"), "L");
        let nested = Lineage { depth: 1, root_session: Some("R".into()) };
        assert_eq!(nested.root_of("L"), "R");
    }

    #[test]
    fn depth_gate() {
        assert!(can_spawn_at_depth(1, 0));
        assert!(!can_spawn_at_depth(1, 1));
        assert!(!can_spawn_at_depth(0, 0));
    }
}
