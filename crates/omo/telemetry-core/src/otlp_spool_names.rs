//! Strict names shared by the spool and the deliberately narrow opt-out purge.
use crate::otlp_spool_file::Signal;

pub const QUARANTINE_DIR: &str = "quarantine";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpoolEntry {
    Active {
        name: String,
        signal: Signal,
        pid: u32,
        boot_id: String,
    },
    Batch {
        name: String,
        signal: Signal,
        batch_id: String,
    },
    Sending {
        name: String,
        signal: Signal,
        batch_id: String,
        claimer: String,
        lease_expires_ms: u64,
    },
    Lock {
        name: String,
        target: String,
    },
    Other {
        name: String,
    },
}
impl SpoolEntry {
    pub fn name(&self) -> &str {
        match self {
            Self::Active { name, .. }
            | Self::Batch { name, .. }
            | Self::Sending { name, .. }
            | Self::Lock { name, .. }
            | Self::Other { name } => name,
        }
    }
}
fn hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn digits(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit())
}
fn uuid7(value: &str) -> bool {
    let parts: Vec<_> = value.split('-').collect();
    parts.len() == 5
        && hex(parts[0], 8)
        && hex(parts[1], 4)
        && hex(parts[2], 4)
        && parts[2].starts_with('7')
        && hex(parts[3], 4)
        && hex(parts[4], 12)
}
pub fn parse_spool_name(name: &str) -> SpoolEntry {
    let other = || SpoolEntry::Other { name: name.into() };
    for signal in [Signal::Logs, Signal::Traces] {
        if let Some(rest) = name
            .strip_prefix(&format!("active-{signal}-"))
            .and_then(|s| s.strip_suffix(".otlp"))
            && let Some((pid, boot)) = rest.split_once('-')
                && digits(pid) && hex(boot, 32)
                    && let Ok(pid) = pid.parse() {
                        return SpoolEntry::Active {
                            name: name.into(),
                            signal,
                            pid,
                            boot_id: boot.into(),
                        };
                    }
        if let Some(rest) = name.strip_prefix(&format!("batch-{signal}-")) {
            if let Some(id) = rest.strip_suffix(".otlp")
                && uuid7(id) {
                    return SpoolEntry::Batch {
                        name: name.into(),
                        signal,
                        batch_id: id.into(),
                    };
                }
            if let Some((id, tail)) = rest.split_once(".sending-")
                && let Some((boot, lease)) = tail.split_once('-')
                    && uuid7(id) && hex(boot, 32) && digits(lease)
                        && let Ok(lease_expires_ms) = lease.parse() {
                            return SpoolEntry::Sending {
                                name: name.into(),
                                signal,
                                batch_id: id.into(),
                                claimer: boot.into(),
                                lease_expires_ms,
                            };
                        }
        }
    }
    if let Some(target) = name.strip_suffix(".lock") {
        // A lock of a lock is not a spool name. Bound recursion before parsing.
        if !target.ends_with(".lock")
            && !matches!(parse_spool_name(target), SpoolEntry::Other { .. })
        {
            return SpoolEntry::Lock {
                name: name.into(),
                target: target.into(),
            };
        }
    }
    other()
}
pub fn is_spool_file_name(name: &str) -> bool {
    !matches!(parse_spool_name(name), SpoolEntry::Other { .. })
}
pub fn active_name(signal: Signal, pid: u32, boot_id: &str) -> String {
    format!("active-{signal}-{pid}-{boot_id}.otlp")
}
pub fn batch_name(signal: Signal, batch_id: &str) -> String {
    format!("batch-{signal}-{batch_id}.otlp")
}
pub fn sending_name(
    signal: Signal,
    batch_id: &str,
    boot_id: &str,
    lease_expires_ms: u64,
) -> String {
    format!("batch-{signal}-{batch_id}.sending-{boot_id}-{lease_expires_ms}")
}
pub fn lock_name(target: &str) -> String {
    format!("{target}.lock")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_builders_names_are_owned() {
        let active = active_name(Signal::Logs, 42, &"a".repeat(32));
        let batch = batch_name(Signal::Traces, "00000000-0000-7000-8000-000000000000");
        for name in [&active, &batch] {
            assert!(is_spool_file_name(name));
            assert!(is_spool_file_name(&lock_name(name)));
            assert!(!is_spool_file_name(&lock_name(&lock_name(name))));
        }
        for name in [
            "notes.otlp",
            "../active-logs-1-a.otlp",
            "batch-logs-00000000-0000-4000-8000-000000000000.otlp",
            "active-logs-1-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA.otlp",
        ] {
            assert!(!is_spool_file_name(name));
        }
    }
}
