use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::Path;
use std::sync::{LazyLock, Mutex};
use std::time::UNIX_EPOCH;

use crate::clock::now_ms;

pub static LOCK_OWNER_INSTANCE_ID: LazyLock<String> = LazyLock::new(|| uuid::Uuid::new_v4().to_string());
static HELD_LOCK_RECORDS: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Mutex::default);

#[derive(Debug, Clone)]
pub struct ObservedLockRecord {
    pub content: String,
    pub modified_at_epoch_ms: i64,
}

struct LockOwner<'a> {
    owner_pid: i64,
    acquired_at_epoch_ms: i64,
    instance_id: Option<&'a str>,
}

pub fn build_lock_record(owner_tag: &str) -> String {
    format!("{owner_tag} {}\n{}\n{}\n{}\n", uuid::Uuid::new_v4(), std::process::id(), now_ms(), *LOCK_OWNER_INSTANCE_ID)
}

pub fn mark_lock_record_held(content: &str) {
    HELD_LOCK_RECORDS.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(content.to_owned());
}

pub fn mark_lock_record_released(content: &str) {
    HELD_LOCK_RECORDS.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(content);
}

pub fn is_path_absence_error(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::NotFound || error.kind() == io::ErrorKind::NotADirectory
        || (!cfg!(windows) && error.raw_os_error() == Some(libc::ENOTDIR))
}

fn parse_lock_owner(content: &str) -> Option<LockOwner<'_>> {
    let lines: Vec<_> = content.split('\n').map(|line| line.strip_suffix('\r').unwrap_or(line))
        .filter(|line| !line.is_empty()).collect();
    if lines.len() != 3 && lines.len() != 4 {
        return None;
    }
    let owner_pid = parse_int_prefix(lines[1])?;
    let acquired_at_epoch_ms = parse_int_prefix(lines[2])?;
    (owner_pid > 0 && acquired_at_epoch_ms > 0).then_some(LockOwner {
        owner_pid, acquired_at_epoch_ms, instance_id: lines.get(3).copied(),
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
    let modified_at_epoch_ms = i64::try_from(modified.duration_since(UNIX_EPOCH).ok()?.as_millis()).ok()?;
    Some(ObservedLockRecord { content, modified_at_epoch_ms })
}

pub fn is_lock_record_stale(record: &ObservedLockRecord, stale_after_ms: i64) -> bool {
    let Some(owner) = parse_lock_owner(&record.content) else {
        return now_ms() - record.modified_at_epoch_ms > stale_after_ms;
    };
    if let Some(instance_id) = owner.instance_id
        && owner.owner_pid == i64::from(std::process::id())
    {
        return instance_id != LOCK_OWNER_INSTANCE_ID.as_str()
            || !HELD_LOCK_RECORDS.lock().unwrap_or_else(std::sync::PoisonError::into_inner).contains(&record.content);
    }
    if super::locks::is_pid_alive(owner.owner_pid) {
        return false;
    }
    if owner.instance_id.is_some() {
        return true;
    }
    now_ms() - owner.acquired_at_epoch_ms > stale_after_ms
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_record_is_live_only_while_held() {
        let record = ObservedLockRecord { content: build_lock_record("owner"), modified_at_epoch_ms: 1 };
        mark_lock_record_held(&record.content);
        assert!(!is_lock_record_stale(&record, 0));
        mark_lock_record_released(&record.content);
        assert!(is_lock_record_stale(&record, i64::MAX));
    }

    #[test]
    fn stale_rules_cover_legacy_torn_dead_and_recycled_records() {
        let cases = [
            (String::new(), 1, true),
            (String::new(), now_ms(), false),
            (format!("legacy\n{}\n1\n", std::process::id()), 1, false),
            ("legacy\n2147483647\n1\n".to_owned(), 1, true),
            (format!("owner\n2147483647\n{}\nforeign\n", now_ms()), 1, true),
            (format!("owner\n{}\n{}\nforeign\n", std::process::id(), now_ms()), 1, true),
        ];
        for (content, modified_at_epoch_ms, expected) in cases {
            assert_eq!(is_lock_record_stale(&ObservedLockRecord { content, modified_at_epoch_ms }, 300_000), expected);
        }
    }
}
