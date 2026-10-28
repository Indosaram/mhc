//! `task.isolation` settings (config-keys.md) and the isolation base directory.
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Environment variable overriding the isolation base directory (absolute paths only).
pub const WORKTREE_DIR_ENV: &str = "MAHO_WORKTREE_DIR";

/// The configured `task.isolation.backend`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BackendChoice {
    /// `auto`, `rcopy`, `worktree`: a git worktree inside git, a copy outside.
    Auto,
    /// `copy`: always a recursive copy.
    Copy,
    /// A copy-on-write backend of the original (`apfs`, `btrfs`, ...) that this build does not
    /// implement; behaves like `Auto` and says so.
    Unsupported(String),
}

/// `task.isolation.merge`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MergeMode {
    /// Capture a patch and `git apply` it to the parent working tree.
    Patch,
    /// Commit onto `mhc/task/<id>` and cherry-pick it onto the parent's HEAD.
    Branch,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IsolationSettings {
    pub backend: BackendChoice,
    /// Apply a successful child's changes to the parent (`task.isolation.apply`).
    pub apply: bool,
    pub merge: MergeMode,
}

impl Default for IsolationSettings {
    fn default() -> Self {
        Self {
            backend: BackendChoice::Auto,
            apply: true,
            merge: MergeMode::Patch,
        }
    }
}

impl IsolationSettings {
    /// Reads `view.task.isolation`; absent or unknown values keep their defaults.
    pub fn from_view(view: Option<&Value>) -> Self {
        let mut settings = Self::default();
        let Some(isolation) = view
            .and_then(|v| v.get("task"))
            .and_then(|t| t.get("isolation"))
        else {
            return settings;
        };
        if let Some(backend) = isolation.get("backend").and_then(Value::as_str) {
            settings.backend = match backend.trim() {
                "auto" | "rcopy" | "worktree" | "" => BackendChoice::Auto,
                "copy" => BackendChoice::Copy,
                other => BackendChoice::Unsupported(other.to_owned()),
            };
        }
        if let Some(apply) = isolation.get("apply").and_then(Value::as_bool) {
            settings.apply = apply;
        }
        if let Some(merge) = isolation.get("merge").and_then(Value::as_str) {
            settings.merge = match merge.trim() {
                "branch" => MergeMode::Branch,
                _ => MergeMode::Patch,
            };
        }
        settings
    }
}

/// Where isolations live: `$MAHO_WORKTREE_DIR` (absolute) or `<agent_dir>/wt`, else the temp dir.
pub fn base_dir(agent_dir: Option<&Path>) -> PathBuf {
    if let Some(dir) = std::env::var_os(WORKTREE_DIR_ENV).map(PathBuf::from)
        && dir.is_absolute()
    {
        return dir;
    }
    agent_dir.map_or_else(|| std::env::temp_dir().join("mhc-wt"), |dir| dir.join("wt"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_the_task_isolation_section() {
        assert_eq!(
            IsolationSettings::from_view(None),
            IsolationSettings::default()
        );
        let view =
            json!({"task": {"isolation": {"backend": "copy", "apply": false, "merge": "branch"}}});
        let settings = IsolationSettings::from_view(Some(&view));
        assert_eq!(settings.backend, BackendChoice::Copy);
        assert!(!settings.apply);
        assert_eq!(settings.merge, MergeMode::Branch);
        let view = json!({"task": {"isolation": {"backend": "btrfs", "merge": "weird"}}});
        let settings = IsolationSettings::from_view(Some(&view));
        assert_eq!(settings.backend, BackendChoice::Unsupported("btrfs".into()));
        assert_eq!(settings.merge, MergeMode::Patch);
        assert_eq!(
            IsolationSettings::from_view(Some(
                &json!({"task": {"isolation": {"backend": "rcopy"}}})
            ))
            .backend,
            BackendChoice::Auto
        );
    }
}
