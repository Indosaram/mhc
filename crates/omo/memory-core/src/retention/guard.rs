//! Lazy scheduler-locked reservation & run-id high-water guard for retention (design H1, J0, R3).

use std::collections::BTreeSet;
use std::path::Path;

use serde_json::Value;

use crate::identity::layout::MemoryIdentityPaths;
use crate::locks::acquire::{AcquireLockError, AcquireLockOptions, WithLockError, with_lock};
use crate::locks::domains::reflection_scheduler_lock_path;
use crate::locks::lock_record::{CreateLockRecordOptions, create_lock_record};
use crate::reflection::run_id_high_water::{
    RunIdHighWaterPaths, record_reflection_run_id_mark, scan_reflection_run_high_water,
};

#[derive(Debug, Clone, Default)]
pub struct ReflectionRetentionGuard {
    /// Highest run number on disk (`H1 Guard 1`): trace carrying it is never removed.
    pub high_water: u64,
    pub reserved: BTreeSet<String>,
    /// Conversations named by `active.lock` / `pending.json`: journals never touched.
    pub reserved_conversation_ids: BTreeSet<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Reservations {
    pub run_ids: BTreeSet<String>,
    pub conversation_ids: BTreeSet<String>,
}

/// Lock-free read of run ids & conversation ids in `active.lock` and `pending.json`.
pub fn read_reservations(reflection_dir: &Path) -> Reservations {
    let mut out = Reservations::default();
    for name in ["active.lock", "pending.json"] {
        let Ok(raw) = std::fs::read_to_string(reflection_dir.join(name)) else {
            continue;
        };
        let Ok(val) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        if let Some(run_id) = val.get("runId").and_then(Value::as_str) {
            out.run_ids.insert(run_id.to_owned());
        }
        if let Some(ids) = val
            .get("request")
            .and_then(|r| r.get("conversationIds"))
            .and_then(Value::as_array)
        {
            for id in ids {
                if let Some(s) = id.as_str() {
                    out.conversation_ids.insert(s.to_owned());
                }
            }
        }
    }
    out
}

pub fn reserved_run_ids(reflection_dir: &Path) -> BTreeSet<String> {
    read_reservations(reflection_dir).run_ids
}

/// Reads reservations and high-water under `reflection-scheduler` lock (`wait_timeout_ms: 0`),
/// raising durable `run-id.json` mark before anything is removed.
pub fn read_reflection_retention_guard(
    paths: &MemoryIdentityPaths,
) -> Result<Option<ReflectionRetentionGuard>, String> {
    std::fs::create_dir_all(&paths.locks).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&paths.locks, std::fs::Permissions::from_mode(0o700));
    }
    let record = create_lock_record("reflection-scheduler", CreateLockRecordOptions::default())
        .map_err(|e| e.to_string())?;
    let lock_path = reflection_scheduler_lock_path(&paths.locks);
    let opts = AcquireLockOptions {
        wait_timeout_ms: Some(0),
        retry_delay_ms: None,
        cancellation: None,
    };

    match with_lock(
        &lock_path,
        &record,
        &opts,
        || -> Result<ReflectionRetentionGuard, String> {
            let high_water = scan_reflection_run_high_water(&RunIdHighWaterPaths {
                reflection: paths.reflection.clone(),
                reflection_sessions: paths.reflection_sessions.clone(),
                worktrees: paths.worktrees.clone(),
            });
            if high_water > 0 {
                record_reflection_run_id_mark(&paths.reflection, high_water)
                    .map_err(|e| e.to_string())?;
            }
            let reservations = read_reservations(&paths.reflection);
            Ok(ReflectionRetentionGuard {
                high_water,
                reserved: reservations.run_ids,
                reserved_conversation_ids: reservations.conversation_ids,
            })
        },
    ) {
        Ok(guard) => Ok(Some(guard)),
        Err(WithLockError::Acquire(AcquireLockError::Contention(_))) => Ok(None),
        Err(WithLockError::Acquire(err)) => Err(err.to_string()),
        Err(WithLockError::User(err)) => Err(err),
    }
}

/// Guard initialized lazily only when a lock-free scan discovers a removal candidate (`R3`).
pub struct LazyRetentionGuard<'a> {
    paths: &'a MemoryIdentityPaths,
    cached: Option<Option<ReflectionRetentionGuard>>,
    pub value: Option<ReflectionRetentionGuard>,
    pub busy: bool,
}

impl<'a> LazyRetentionGuard<'a> {
    pub fn new(paths: &'a MemoryIdentityPaths) -> Self {
        Self {
            paths,
            cached: None,
            value: None,
            busy: false,
        }
    }

    pub fn get(&mut self) -> Result<Option<&ReflectionRetentionGuard>, String> {
        if self.cached.is_none() {
            let guard = read_reflection_retention_guard(self.paths)?;
            self.busy = guard.is_none();
            self.value = guard.clone();
            self.cached = Some(guard);
        }
        Ok(self.value.as_ref())
    }
}
