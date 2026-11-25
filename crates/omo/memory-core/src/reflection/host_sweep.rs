//! Host-wide identity cursor pass (`runtime-sweep.json` under `runtime-sweep.lock`) (#10144, #12517).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::identity::layout::{AGENTS_DIRNAME, REPO_DIRNAME, RUNTIME_DIRNAME};
use crate::locks::acquire::{AcquireLockError, AcquireLockOptions, WithLockError, with_lock};
use crate::locks::lock_record::create_lock_record;
use crate::support::random::random_uuid;
use crate::support::time::{format_rfc3339_millis, now_millis};

pub const HOST_SWEEP_STATE_FILE: &str = "runtime-sweep.json";
pub const HOST_SWEEP_LOCK_FILE: &str = "runtime-sweep.lock";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostSweepState {
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostSweepIdentity {
    pub name: String,
    pub root: PathBuf,
    pub runtime_dir: PathBuf,
    pub has_repo: bool,
}

pub type HostSweepStep = Arc<dyn Fn(&HostSweepIdentity) -> Vec<String> + Send + Sync>;

#[derive(Clone, Default)]
pub struct HostSweepOptions<'a> {
    pub memory_root: PathBuf,
    pub steps: Vec<HostSweepStep>,
    /// Halt after this many identities (bounded driver); `None` visits every identity once.
    pub max_identities: Option<usize>,
    /// Halt once this much time (ms) has elapsed (checked between identities).
    pub budget_ms: Option<i64>,
    pub now_ms: Option<&'a dyn Fn() -> i64>,
    pub aborted: Option<&'a dyn Fn() -> bool>,
    /// Walk and run steps without advancing the cursor.
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostSweepResult {
    Busy,
    Swept {
        visited: Vec<String>,
        completed: bool,
        lines: Vec<String>,
    },
}

fn list_runtime_identities(memory_root: &Path) -> std::io::Result<Vec<HostSweepIdentity>> {
    let agents_root = memory_root.join(AGENTS_DIRNAME);
    let entries = match std::fs::read_dir(&agents_root) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| !name.starts_with('.'))
        .collect();
    names.sort();

    let mut identities = Vec::new();
    for name in names {
        let root = agents_root.join(&name);
        let runtime_dir = root.join(RUNTIME_DIRNAME);
        if !runtime_dir.is_dir() {
            continue;
        }
        let has_repo = root.join(REPO_DIRNAME).is_dir();
        identities.push(HostSweepIdentity {
            name,
            root,
            runtime_dir,
            has_repo,
        });
    }
    Ok(identities)
}

pub fn read_host_sweep_state(memory_root: &Path) -> HostSweepState {
    let Ok(raw) = std::fs::read_to_string(memory_root.join(HOST_SWEEP_STATE_FILE)) else {
        return HostSweepState {
            version: 1,
            cursor: None,
            completed_at: None,
        };
    };
    serde_json::from_str::<HostSweepState>(&raw)
        .map(|mut s| {
            s.version = 1;
            s
        })
        .unwrap_or(HostSweepState {
            version: 1,
            cursor: None,
            completed_at: None,
        })
}

fn write_host_sweep_state(memory_root: &Path, state: &HostSweepState) -> std::io::Result<()> {
    std::fs::create_dir_all(memory_root)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(memory_root, std::fs::Permissions::from_mode(0o700));
    }
    let target = memory_root.join(HOST_SWEEP_STATE_FILE);
    let temp = memory_root.join(format!("{HOST_SWEEP_STATE_FILE}.tmp-{}", random_uuid()));
    let json = serde_json::to_string(state)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    if let Err(err) = std::fs::write(&temp, format!("{json}\n")) {
        let _ = std::fs::remove_file(&temp);
        return Err(err);
    }
    if let Err(err) = std::fs::rename(&temp, &target) {
        let _ = std::fs::remove_file(&temp);
        return Err(err);
    }
    Ok(())
}

fn resume_order(
    identities: Vec<HostSweepIdentity>,
    cursor: Option<&str>,
    unbounded: bool,
) -> (Vec<HostSweepIdentity>, Vec<HostSweepIdentity>) {
    let Some(cursor) = cursor else {
        return (identities, Vec::new());
    };
    let mut rest = Vec::new();
    let mut wrap = Vec::new();
    for identity in identities {
        if identity.name.as_str() > cursor {
            rest.push(identity);
        } else if unbounded {
            wrap.push(identity);
        }
    }
    (rest, wrap)
}

/// Visits identities from the persisted cursor under `runtime-sweep.lock` (`wait_timeout_ms: 0`).
pub fn sweep_host_identities(options: &HostSweepOptions<'_>) -> Result<HostSweepResult, String> {
    let now_fn = options.now_ms.unwrap_or(&now_millis);
    let started_at = now_fn();
    let record =
        create_lock_record("runtime-sweep", Default::default()).map_err(|e| e.to_string())?;
    let lock_path = options.memory_root.join(HOST_SWEEP_LOCK_FILE);
    let acquire_opts = AcquireLockOptions {
        wait_timeout_ms: Some(0),
        retry_delay_ms: None,
        cancellation: None,
    };

    match with_lock(
        &lock_path,
        &record,
        &acquire_opts,
        || -> Result<HostSweepResult, String> {
            let state = read_host_sweep_state(&options.memory_root);
            let unbounded = options.max_identities.is_none() && options.budget_ms.is_none();
            let identities =
                list_runtime_identities(&options.memory_root).map_err(|e| e.to_string())?;
            let (rest, wrap) = resume_order(identities, state.cursor.as_deref(), unbounded);

            let mut visited = Vec::new();
            let mut lines = Vec::new();
            let mut visit = |identity: &HostSweepIdentity| -> bool {
                if options.aborted.is_some_and(|aborted| aborted()) {
                    return false;
                }
                if let Some(max) = options.max_identities
                    && visited.len() >= max
                {
                    return false;
                }
                if let Some(budget) = options.budget_ms
                    && !visited.is_empty()
                    && now_fn() - started_at >= budget
                {
                    return false;
                }
                for step in &options.steps {
                    lines.extend(step(identity));
                }
                visited.push(identity.name.clone());
                true
            };

            let mut reached_end = true;
            for identity in &rest {
                if !visit(identity) {
                    reached_end = false;
                    break;
                }
                if !options.dry_run {
                    write_host_sweep_state(
                        &options.memory_root,
                        &HostSweepState {
                            version: 1,
                            cursor: Some(identity.name.clone()),
                            completed_at: None,
                        },
                    )
                    .map_err(|e| e.to_string())?;
                }
            }
            if reached_end && !options.dry_run {
                write_host_sweep_state(
                    &options.memory_root,
                    &HostSweepState {
                        version: 1,
                        cursor: None,
                        completed_at: Some(format_rfc3339_millis(now_fn())),
                    },
                )
                .map_err(|e| e.to_string())?;
            }
            if reached_end {
                for identity in &wrap {
                    if !visit(identity) {
                        break;
                    }
                }
            }
            Ok(HostSweepResult::Swept {
                visited,
                completed: reached_end,
                lines,
            })
        },
    ) {
        Ok(res) => Ok(res),
        Err(WithLockError::Acquire(AcquireLockError::Contention(_))) => Ok(HostSweepResult::Busy),
        Err(WithLockError::Acquire(err)) => Err(err.to_string()),
        Err(WithLockError::User(err)) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_cursor_advances_and_resumes_across_calls() {
        let dir = tempfile::tempdir().unwrap();
        for id in ["alpha", "beta", "gamma"] {
            std::fs::create_dir_all(dir.path().join("agents").join(id).join("runtime")).unwrap();
        }
        let step: HostSweepStep = Arc::new(|id| vec![format!("visited {}", id.name)]);

        let first = sweep_host_identities(&HostSweepOptions {
            memory_root: dir.path().to_path_buf(),
            steps: vec![step.clone()],
            max_identities: Some(2),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            first,
            HostSweepResult::Swept {
                visited: vec!["alpha".into(), "beta".into()],
                completed: false,
                lines: vec!["visited alpha".into(), "visited beta".into()],
            }
        );

        let second = sweep_host_identities(&HostSweepOptions {
            memory_root: dir.path().to_path_buf(),
            steps: vec![step],
            max_identities: Some(2),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            second,
            HostSweepResult::Swept {
                visited: vec!["gamma".into()],
                completed: true,
                lines: vec!["visited gamma".into()],
            }
        );
    }
}
