//! Linux host & cgroup v2 memory headroom probe (`thread/wake/memory-headroom.ts`).

use std::path::Path;

pub const WAKE_MEMORY_FLOOR_BYTES: u64 = 1024 * 1024 * 1024;
pub const WAKE_SESSION_COST_BYTES: u64 = 409 * 1024 * 1024;
const FLOOR_SHARE_OF_LIMIT: u64 = 4;

fn readable(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

fn cgroup_memory_with<F>(read_fn: &F) -> Option<(u64, u64)>
where
    F: Fn(&Path) -> Option<String>,
{
    let cgroup_text = read_fn(Path::new("/proc/self/cgroup"))?;
    let entry = cgroup_text.lines().find(|line| line.starts_with("0::"))?;
    let rel = entry.trim_start_matches("0::").trim().trim_end_matches('/');
    let base = format!("/sys/fs/cgroup{rel}");
    let max_text = read_fn(Path::new(&format!("{base}/memory.max")))?;
    let max_trimmed = max_text.trim();
    if max_trimmed == "max" {
        return None;
    }
    let limit = max_trimmed.parse::<u64>().ok()?;
    let current_text = read_fn(Path::new(&format!("{base}/memory.current")))?;
    let current = current_text.trim().parse::<u64>().ok()?;
    Some((limit, current))
}

fn meminfo_with<F>(read_fn: &F, field: &str) -> Option<u64>
where
    F: Fn(&Path) -> Option<String>,
{
    let prefix = format!("{field}:");
    let text = read_fn(Path::new("/proc/meminfo"))?;
    let line = text.lines().find(|row| row.starts_with(&prefix))?;
    let kib_str = line[prefix.len()..].split_whitespace().next()?;
    let kib = kib_str.parse::<u64>().ok()?;
    Some(kib * 1024)
}

pub fn read_host_memory_headroom() -> Option<u64> {
    if cfg!(not(target_os = "linux")) {
        return None;
    }
    read_host_memory_headroom_with(&readable)
}

pub fn read_host_memory_headroom_with<F>(read_fn: &F) -> Option<u64>
where
    F: Fn(&Path) -> Option<String>,
{
    let mut min_val: Option<u64> = None;
    if let Some((limit, current)) = cgroup_memory_with(read_fn) {
        let room = limit.saturating_sub(current);
        min_val = Some(min_val.map_or(room, |m| m.min(room)));
    }
    if let Some(avail) = meminfo_with(read_fn, "MemAvailable") {
        min_val = Some(min_val.map_or(avail, |m| m.min(avail)));
    }
    min_val
}

pub fn read_wake_memory_floor() -> u64 {
    if cfg!(not(target_os = "linux")) {
        return WAKE_MEMORY_FLOOR_BYTES;
    }
    read_wake_memory_floor_with(&readable)
}

pub fn read_wake_memory_floor_with<F>(read_fn: &F) -> u64
where
    F: Fn(&Path) -> Option<String>,
{
    let mut min_size: Option<u64> = None;
    if let Some((limit, _)) = cgroup_memory_with(read_fn) {
        min_size = Some(min_size.map_or(limit, |m| m.min(limit)));
    }
    if let Some(total) = meminfo_with(read_fn, "MemTotal") {
        min_size = Some(min_size.map_or(total, |m| m.min(total)));
    }
    let Some(size) = min_size else {
        return WAKE_MEMORY_FLOOR_BYTES;
    };
    let share = size / FLOOR_SHARE_OF_LIMIT;
    WAKE_MEMORY_FLOOR_BYTES.min(WAKE_SESSION_COST_BYTES.max(share))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_headroom_and_floor_from_meminfo_and_cgroup() {
        let reader = |p: &Path| -> Option<String> {
            match p.to_str()? {
                "/proc/self/cgroup" => Some("0::/user.slice\n".into()),
                "/sys/fs/cgroup/user.slice/memory.max" => {
                    Some(format!("{}\n", 2 * 1024 * 1024 * 1024u64))
                }
                "/sys/fs/cgroup/user.slice/memory.current" => {
                    Some(format!("{}\n", 1024 * 1024 * 1024u64))
                }
                "/proc/meminfo" => {
                    Some("MemTotal:        8000000 kB\nMemAvailable:    1500000 kB\n".into())
                }
                _ => None,
            }
        };
        assert_eq!(
            read_host_memory_headroom_with(&reader),
            Some(1024 * 1024 * 1024)
        );
        assert_eq!(read_wake_memory_floor_with(&reader), 512 * 1024 * 1024);
    }
}
