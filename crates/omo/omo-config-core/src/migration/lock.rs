use std::cell::{Cell, RefCell};

use serde_json::{Value, json};

use crate::internal::posix_path::{posix_join, to_posix_path};
use crate::loader::paths::resolve_home_dir;
use crate::migration::types::{
    DEFAULT_LEASE_DURATION_MS, GUARD_LEASE_DURATION_MS, LIVE_OWNER_STALE_LEASE_MULTIPLIER,
    MUTATION_GUARD_RETRY_DELAYS_MS, MigrationClock, MigrationEnvironment, MigrationError,
    MigrationFileSystem,
};
use crate::writer::types::FsErrorKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LockRecord {
    pub lease_expires_at: i64,
    pub pid: u32,
}

pub struct MigrationLock<'a> {
    pub file_system: &'a dyn MigrationFileSystem,
    pub env: MigrationEnvironment,
    pub path: String,
    pub pid: u32,
    pub lease_duration_ms: i64,
    pub clock: &'a dyn MigrationClock,
    pub is_alive: &'a (dyn Fn(u32) -> bool + 'a),
    pub owned_content: RefCell<String>,
    pub active: Cell<bool>,
}

impl<'a> MigrationLock<'a> {
    pub fn renew(&self) -> Result<(), MigrationError> {
        let guard_content = acquire_mutation_guard(
            &self.env,
            self.file_system,
            self.pid,
            self.clock,
            self.is_alive,
        )?;
        let guard_content = match guard_content {
            Some(content) => content,
            None => return Err(MigrationError::lock("Migration lock mutation is busy")),
        };

        let renewed_content = lease_content(self.pid, self.clock.now(), self.lease_duration_ms);
        let owned = self.owned_content.borrow().clone();
        let replaced =
            self.file_system
                .replace_if_contents_match(&self.path, &owned, &renewed_content)?;
        release_mutation_guard(&self.env, self.file_system, &guard_content);

        if !replaced {
            return Err(MigrationError::lock("Migration lock ownership was lost"));
        }
        *self.owned_content.borrow_mut() = renewed_content;
        Ok(())
    }

    pub fn release(&self) -> Result<(), MigrationError> {
        if !self.active.get() {
            return Ok(());
        }
        self.active.set(false);
        let guard_content = acquire_mutation_guard(
            &self.env,
            self.file_system,
            self.pid,
            self.clock,
            self.is_alive,
        )?;
        let owned = self.owned_content.borrow().clone();
        match guard_content {
            Some(guard) => {
                let _ = self
                    .file_system
                    .remove_if_contents_match(&self.path, &owned);
                release_mutation_guard(&self.env, self.file_system, &guard);
            }
            None => {
                let _ = self
                    .file_system
                    .remove_if_contents_match(&self.path, &owned);
            }
        }
        Ok(())
    }
}

impl<'a> Drop for MigrationLock<'a> {
    fn drop(&mut self) {
        let _ = self.release();
    }
}

pub fn migration_lock_path(env: &MigrationEnvironment) -> String {
    to_posix_path(&posix_join(&[
        &resolve_home_dir(env),
        ".maho",
        ".migration.lock",
    ]))
}

pub fn mutation_guard_path(env: &MigrationEnvironment) -> String {
    format!("{}.guard", migration_lock_path(env))
}

pub fn parse_lock_record(content: &str) -> Option<LockRecord> {
    let parsed: Value = serde_json::from_str(content).ok()?;
    let obj = parsed.as_object()?;
    let pid_val = obj.get("pid")?.as_i64()?;
    if pid_val < 1 {
        return None;
    }
    let lease_expires_at = obj.get("leaseExpiresAt")?.as_i64()?;
    Some(LockRecord {
        lease_expires_at,
        pid: pid_val as u32,
    })
}

pub fn lease_content(pid: u32, now: i64, duration: i64) -> String {
    format!(
        "{}\n",
        serde_json::to_string(&json!({
            "leaseExpiresAt": now + duration,
            "pid": pid,
        }))
        .unwrap()
    )
}

pub fn is_reclaimable(
    record: &LockRecord,
    now: i64,
    is_alive: &dyn Fn(u32) -> bool,
    lease_duration_ms: i64,
) -> bool {
    if record.lease_expires_at > now {
        return false;
    }
    if !is_alive(record.pid) {
        return true;
    }
    record.lease_expires_at + lease_duration_ms * LIVE_OWNER_STALE_LEASE_MULTIPLIER <= now
}

fn sleep_sync(milliseconds: u64) {
    std::thread::sleep(std::time::Duration::from_millis(milliseconds));
}

pub fn acquire_mutation_guard(
    env: &MigrationEnvironment,
    file_system: &dyn MigrationFileSystem,
    pid: u32,
    clock: &dyn MigrationClock,
    is_alive: &dyn Fn(u32) -> bool,
) -> Result<Option<String>, MigrationError> {
    let path = mutation_guard_path(env);
    let attempts_count = MUTATION_GUARD_RETRY_DELAYS_MS.len();
    for attempt in 0..=attempts_count {
        let content = lease_content(pid, clock.now(), GUARD_LEASE_DURATION_MS);
        match file_system.write_exclusive(&path, &content) {
            Ok(()) => return Ok(Some(content)),
            Err(error) => {
                if error.kind != FsErrorKind::Exists {
                    return Err(MigrationError::Fs(error.message));
                }
            }
        }

        match file_system.read(&path) {
            Ok(observed_content) => {
                let observed = parse_lock_record(&observed_content);
                let reclaimable = match observed {
                    Some(rec) => {
                        is_reclaimable(&rec, clock.now(), is_alive, GUARD_LEASE_DURATION_MS)
                    }
                    None => true,
                };
                if reclaimable {
                    let _ = file_system.remove_if_contents_match(&path, &observed_content);
                }
            }
            Err(error) => {
                if error.kind != FsErrorKind::NotFound {
                    return Err(MigrationError::Fs(error.message));
                }
            }
        }

        if let Some(&delay_ms) = MUTATION_GUARD_RETRY_DELAYS_MS.get(attempt) {
            sleep_sync(delay_ms);
        }
    }
    Ok(None)
}

pub fn release_mutation_guard(
    env: &MigrationEnvironment,
    file_system: &dyn MigrationFileSystem,
    content: &str,
) {
    let path = mutation_guard_path(env);
    let _ = file_system.remove_if_contents_match(&path, content);
}

pub struct AcquireMigrationLockInput<'a> {
    pub clock: &'a dyn MigrationClock,
    pub env: &'a MigrationEnvironment,
    pub file_system: &'a dyn MigrationFileSystem,
    pub is_alive: &'a (dyn Fn(u32) -> bool + 'a),
    pub lease_duration_ms: Option<i64>,
    pub pid: u32,
}

pub fn acquire_migration_lock<'a>(
    input: AcquireMigrationLockInput<'a>,
) -> Result<Option<MigrationLock<'a>>, MigrationError> {
    let lease_duration_ms = input.lease_duration_ms.unwrap_or(DEFAULT_LEASE_DURATION_MS);
    let path = migration_lock_path(input.env);
    let dot_omo = to_posix_path(&posix_join(&[&resolve_home_dir(input.env), ".maho"]));
    input.file_system.mkdirs(&dot_omo)?;

    for _attempt in 0..3 {
        let current_content = lease_content(input.pid, input.clock.now(), lease_duration_ms);
        match input.file_system.write_exclusive(&path, &current_content) {
            Ok(()) => {
                return Ok(Some(MigrationLock {
                    file_system: input.file_system,
                    env: input.env.clone(),
                    path,
                    pid: input.pid,
                    lease_duration_ms,
                    clock: input.clock,
                    is_alive: input.is_alive,
                    owned_content: RefCell::new(current_content),
                    active: Cell::new(true),
                }));
            }
            Err(error) => {
                if error.kind != FsErrorKind::Exists {
                    return Err(MigrationError::Fs(error.message));
                }
            }
        }

        let guard_content = match acquire_mutation_guard(
            input.env,
            input.file_system,
            input.pid,
            input.clock,
            input.is_alive,
        )? {
            Some(guard) => guard,
            None => return Ok(None),
        };

        let observed_content = match input.file_system.read(&path) {
            Ok(content) => content,
            Err(error) => {
                release_mutation_guard(input.env, input.file_system, &guard_content);
                if error.kind == FsErrorKind::NotFound {
                    continue;
                }
                return Err(MigrationError::Fs(error.message));
            }
        };

        let observed = parse_lock_record(&observed_content);
        if let Some(obs) = observed
            && !is_reclaimable(&obs, input.clock.now(), input.is_alive, lease_duration_ms)
        {
            release_mutation_guard(input.env, input.file_system, &guard_content);
            return Ok(None);
        }

        let _ = input
            .file_system
            .remove_if_contents_match(&path, &observed_content);
        release_mutation_guard(input.env, input.file_system, &guard_content);
    }

    Ok(None)
}
