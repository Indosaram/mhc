use std::fs;
use std::io;
use std::path::Path;
use std::time::UNIX_EPOCH;

use memory_core::locks::{
    ProcessLiveness, get_pid_liveness, get_process_start_identity, start_identities_conflict,
};

pub use super::lock_worker_lifetime::{
    LOCK_OWNER_INSTANCE_ID, forget_lock_record, is_worker_lock_released, lock_owner_instance_id,
    mark_lock_record_held, mark_lock_record_released,
};
use crate::clock::now_ms;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedLockRecord {
    pub content: String,
    pub modified_at_epoch_ms: i64,
}

struct LockOwner<'a> {
    owner_pid: i64,
    acquired_at_epoch_ms: i64,
    instance_id: Option<&'a str>,
    process_start: Option<&'a str>,
}

/// Who a lock record names: the owning pid and when that owner took (or was handed) it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LockHolder {
    pub owner_pid: i64,
    pub acquired_at_epoch_ms: i64,
}

/// Line 5 of a lock record: the owner's process-start identity, or `unavailable` when this host
/// cannot read it.
#[must_use]
pub fn lock_process_start_line(pid: u32) -> String {
    get_process_start_identity(pid).unwrap_or_else(|| "unavailable".to_owned())
}

#[must_use]
pub fn build_lock_record(owner_tag: &str) -> String {
    let pid = std::process::id();
    let process_start = lock_process_start_line(pid);
    format!(
        "{owner_tag} {}\n{pid}\n{}\n{}\n{process_start}\n",
        uuid::Uuid::new_v4(),
        now_ms(),
        lock_owner_instance_id()
    )
}

pub fn is_path_absence_error(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::NotFound
        || error.kind() == io::ErrorKind::NotADirectory
        || (!cfg!(windows) && error.raw_os_error() == Some(libc::ENOTDIR))
}

fn parse_lock_owner(content: &str) -> Option<LockOwner<'_>> {
    let lines: Vec<_> = content
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .filter(|line| !line.is_empty())
        .collect();
    if !matches!(lines.len(), 3..=5) {
        return None;
    }
    let owner_pid = parse_int_prefix(lines[1])?;
    let acquired_at_epoch_ms = parse_int_prefix(lines[2])?;
    (owner_pid > 0 && acquired_at_epoch_ms > 0).then_some(LockOwner {
        owner_pid,
        acquired_at_epoch_ms,
        instance_id: lines.get(3).copied(),
        process_start: lines.get(4).copied(),
    })
}

/// The holder a record names, or `None` for a torn or unparseable record.
#[must_use]
pub fn parse_lock_holder(content: &str) -> Option<LockHolder> {
    let owner = parse_lock_owner(content)?;
    Some(LockHolder {
        owner_pid: owner.owner_pid,
        acquired_at_epoch_ms: owner.acquired_at_epoch_ms,
    })
}

fn parse_int_prefix(text: &str) -> Option<i64> {
    let trimmed = text.trim_start();
    let (sign, digits) = match trimmed.as_bytes().first() {
        Some(b'-') => (-1, &trimmed[1..]),
        Some(b'+') => (1, &trimmed[1..]),
        _ => (1, trimmed),
    };
    let end = digits.bytes().take_while(u8::is_ascii_digit).count();
    digits[..end].parse::<i64>().ok()?.checked_mul(sign)
}

pub fn read_lock_record(lock_path: &Path) -> Option<ObservedLockRecord> {
    let content = fs::read_to_string(lock_path).ok()?;
    let modified = fs::metadata(lock_path).ok()?.modified().ok()?;
    let modified_at_epoch_ms =
        i64::try_from(modified.duration_since(UNIX_EPOCH).ok()?.as_millis()).ok()?;
    Some(ObservedLockRecord {
        content,
        modified_at_epoch_ms,
    })
}

pub fn is_lock_record_stale(record: &ObservedLockRecord, stale_after_ms: i64) -> bool {
    let Some(owner) = parse_lock_owner(&record.content) else {
        return now_ms() - record.modified_at_epoch_ms > stale_after_ms;
    };
    let liveness = match u32::try_from(owner.owner_pid) {
        Ok(pid) => get_pid_liveness(pid),
        Err(_) => ProcessLiveness::Dead,
    };
    match liveness {
        ProcessLiveness::Dead => {
            return owner.instance_id.is_some()
                || now_ms() - owner.acquired_at_epoch_ms > stale_after_ms;
        }
        ProcessLiveness::Unknown => return false,
        ProcessLiveness::Alive => {}
    }
    if let Some(recorded_start) = owner.process_start
        && let Ok(pid) = u32::try_from(owner.owner_pid)
        && let Some(actual_start) = get_process_start_identity(pid)
        && start_identities_conflict(recorded_start, &actual_start)
    {
        return true;
    }
    if let Some(instance_id) = owner.instance_id
        && owner.owner_pid == i64::from(std::process::id())
    {
        return is_worker_lock_released(instance_id, &record.content);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::team_state_store::lock_worker_lifetime::LockWorkerScope;

    #[test]
    fn own_record_is_live_only_while_held() {
        let record = ObservedLockRecord {
            content: build_lock_record("owner"),
            modified_at_epoch_ms: 1,
        };
        mark_lock_record_held(&record.content);
        assert!(!is_lock_record_stale(&record, 0));
        mark_lock_record_released(&record.content);
        assert!(is_lock_record_stale(&record, i64::MAX));
        forget_lock_record(&record.content);
    }

    #[test]
    fn stale_rules_cover_legacy_torn_dead_and_recycled_records() {
        let own_pid = std::process::id();
        let conflicting_start = get_process_start_identity(own_pid).map(|start| {
            let scheme = start.split(':').next().unwrap_or("linux-proc-start-ticks");
            format!("{scheme}:0")
        });
        let worker_scope = LockWorkerScope::enter();
        let worker_record = build_lock_record("worker");
        mark_lock_record_held(&worker_record);
        assert!(!is_lock_record_stale(
            &ObservedLockRecord {
                content: worker_record.clone(),
                modified_at_epoch_ms: 1,
            },
            300_000,
        ));
        worker_scope.exit();
        assert!(is_lock_record_stale(
            &ObservedLockRecord {
                content: worker_record.clone(),
                modified_at_epoch_ms: 1,
            },
            300_000,
        ));
        forget_lock_record(&worker_record);

        let mut cases = vec![
            (String::new(), 1, true),
            (String::new(), now_ms(), false),
            (format!("legacy\n{own_pid}\n1\n"), 1, false),
            ("legacy\n2147483647\n1\n".to_owned(), 1, true),
            (
                format!("owner\n2147483647\n{}\nforeign\n", now_ms()),
                1,
                true,
            ),
            (
                format!("owner\n{own_pid}\n{}\nforeign\n", now_ms()),
                1,
                false,
            ),
        ];
        if let Some(start) = conflicting_start {
            cases.push((
                format!("owner\n{own_pid}\n{}\nforeign\n{start}\n", now_ms()),
                1,
                true,
            ));
        }
        for (content, modified_at_epoch_ms, expected) in cases {
            assert_eq!(
                is_lock_record_stale(
                    &ObservedLockRecord {
                        content,
                        modified_at_epoch_ms
                    },
                    300_000
                ),
                expected
            );
        }
    }
}
