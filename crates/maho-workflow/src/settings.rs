//! The `task.dag.*` configuration keys (config-keys.md) as typed settings.
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DagSettings {
    pub max_nodes_per_run: usize,
    pub max_runs_per_session: usize,
    /// Buffered events per live subscriber.
    pub subscriber_ring: usize,
    /// Lease refresh interval of the owning process.
    pub heartbeat_ms: u64,
    pub history_default_limit: usize,
    pub history_max_limit: usize,
    pub retention_days: u64,
    pub max_prompt_bytes: usize,
}

impl Default for DagSettings {
    fn default() -> Self {
        Self {
            max_nodes_per_run: 64,
            max_runs_per_session: 16,
            subscriber_ring: 1000,
            heartbeat_ms: 15_000,
            history_default_limit: 256,
            history_max_limit: 1000,
            retention_days: 7,
            max_prompt_bytes: 262_144,
        }
    }
}

fn positive(dag: &Value, key: &str) -> Option<u64> {
    dag.get(key).and_then(Value::as_u64).filter(|n| *n > 0)
}

fn to_usize(n: u64) -> usize {
    usize::try_from(n).unwrap_or(usize::MAX)
}

impl DagSettings {
    /// Reads `view.task.dag`; absent, zero or non-integer values keep their defaults.
    pub fn from_view(view: Option<&Value>) -> Self {
        let mut settings = Self::default();
        let Some(dag) = view
            .and_then(|view| view.get("task"))
            .and_then(|task| task.get("dag"))
        else {
            return settings;
        };
        if let Some(n) = positive(dag, "max_nodes_per_run") {
            settings.max_nodes_per_run = to_usize(n);
        }
        if let Some(n) = positive(dag, "max_runs_per_session") {
            settings.max_runs_per_session = to_usize(n);
        }
        if let Some(n) = positive(dag, "subscriber_ring") {
            settings.subscriber_ring = to_usize(n);
        }
        if let Some(n) = positive(dag, "heartbeat_ms") {
            settings.heartbeat_ms = n;
        }
        if let Some(n) = positive(dag, "history_default_limit") {
            settings.history_default_limit = to_usize(n);
        }
        if let Some(n) = positive(dag, "history_max_limit") {
            settings.history_max_limit = to_usize(n);
        }
        if let Some(n) = positive(dag, "retention_days") {
            settings.retention_days = n;
        }
        if let Some(n) = positive(dag, "max_prompt_bytes") {
            settings.max_prompt_bytes = to_usize(n);
        }
        settings
    }

    /// An event-history page size: `requested` (default `history_default_limit`) clamped to
    /// `1..=history_max_limit`.
    pub fn history_limit(&self, requested: Option<usize>) -> usize {
        requested
            .unwrap_or(self.history_default_limit)
            .clamp(1, self.history_max_limit.max(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_dag_section_and_ignores_invalid_values() {
        let view = json!({"task": {"dag": {"max_nodes_per_run": 3, "max_runs_per_session": 0,
            "retention_days": 2, "history_max_limit": 10, "max_prompt_bytes": "x"}}});
        let settings = DagSettings::from_view(Some(&view));
        assert_eq!(settings.max_nodes_per_run, 3);
        assert_eq!(settings.max_runs_per_session, 16);
        assert_eq!(settings.retention_days, 2);
        assert_eq!(settings.max_prompt_bytes, 262_144);
        assert_eq!(settings.history_limit(None), 10);
        assert_eq!(settings.history_limit(Some(0)), 1);
        assert_eq!(DagSettings::from_view(None), DagSettings::default());
    }
}
