//! Disk usage measurement (`du` allocated blocks where supported, symlink-safe) (#10144).

use std::path::Path;

use crate::support::time::format_rfc3339_millis;

use super::policy::list_names;

/// `memory.runtime.max_bytes` default per identity (2 GiB); `0` means unbounded.
pub const DEFAULT_RUNTIME_MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const RUNTIME_SIZE_WARN_RATIO: f64 = 0.8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeStoreSize {
    pub store: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeSize {
    pub bytes: u64,
    /// Largest first; `reflection/<x>` and `recall/<x>` split.
    pub stores: Vec<RuntimeStoreSize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeSizeStandingLevel {
    Ok,
    Warn,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeSizeStanding {
    pub level: RuntimeSizeStandingLevel,
    pub percent: Option<u64>,
}

pub fn runtime_size_standing(bytes: u64, max_bytes: u64) -> RuntimeSizeStanding {
    if max_bytes == 0 {
        return RuntimeSizeStanding {
            level: RuntimeSizeStandingLevel::Ok,
            percent: None,
        };
    }
    let percent = ((bytes as f64 / max_bytes as f64) * 100.0).floor() as u64;
    let level = if (bytes as f64) >= (max_bytes as f64) * RUNTIME_SIZE_WARN_RATIO {
        RuntimeSizeStandingLevel::Warn
    } else {
        RuntimeSizeStandingLevel::Ok
    };
    RuntimeSizeStanding {
        level,
        percent: Some(percent),
    }
}

pub fn format_runtime_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KiB", "MiB", "GiB", "TiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0usize;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn charged_bytes(meta: &std::fs::Metadata) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let blocks = meta.blocks();
        if blocks > 0 || meta.len() == 0 {
            return blocks.saturating_mul(512);
        }
    }
    meta.len()
}

#[derive(Debug)]
pub struct WalkAborted;

/// Bytes under `path`, including `path`; missing entries count as zero.
pub fn measure_tree_bytes(
    path: &Path,
    aborted: Option<&dyn Fn() -> bool>,
) -> Result<u64, WalkAborted> {
    if aborted.is_some_and(|f| f()) {
        return Err(WalkAborted);
    }
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(_) => return Ok(0),
    };
    let mut bytes = charged_bytes(&meta);
    if !meta.is_dir() {
        return Ok(bytes);
    }
    for name in list_names(path) {
        bytes = bytes.saturating_add(measure_tree_bytes(&path.join(name), aborted)?);
    }
    Ok(bytes)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeSizeUnknown {
    pub budget_ms: u64,
    pub at: String,
}

pub enum RuntimeSizeBudgetOutcome {
    Size(RuntimeSize),
    Unknown(RuntimeSizeUnknown),
}

pub fn measure_runtime_size(
    runtime_dir: &Path,
    aborted: Option<&dyn Fn() -> bool>,
) -> Result<RuntimeSize, WalkAborted> {
    if aborted.is_some_and(|f| f()) {
        return Err(WalkAborted);
    }
    let root = match std::fs::symlink_metadata(runtime_dir) {
        Ok(m) => m,
        Err(_) => return Ok(RuntimeSize::default()),
    };
    let mut bytes = charged_bytes(&root);
    let mut stores = Vec::new();
    if root.is_dir() {
        for name in list_names(runtime_dir) {
            let path = runtime_dir.join(&name);
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.is_dir() && (name == "reflection" || name == "recall") {
                bytes = bytes.saturating_add(charged_bytes(&meta));
                for child in list_names(&path) {
                    let child_bytes = measure_tree_bytes(&path.join(&child), aborted)?;
                    bytes = bytes.saturating_add(child_bytes);
                    stores.push(RuntimeStoreSize {
                        store: format!("{name}/{child}"),
                        bytes: child_bytes,
                    });
                }
                continue;
            }
            let store_bytes = measure_tree_bytes(&path, aborted)?;
            bytes = bytes.saturating_add(store_bytes);
            stores.push(RuntimeStoreSize {
                store: name,
                bytes: store_bytes,
            });
        }
    }
    stores.sort_by(|l, r| r.bytes.cmp(&l.bytes).then_with(|| l.store.cmp(&r.store)));
    Ok(RuntimeSize { bytes, stores })
}

pub fn measure_runtime_size_within_budget(
    runtime_dir: &Path,
    budget_ms: Option<u64>,
    now_ms: i64,
    external_aborted: Option<&dyn Fn() -> bool>,
) -> RuntimeSizeBudgetOutcome {
    let deadline =
        budget_ms.map(|ms| std::time::Instant::now() + std::time::Duration::from_millis(ms));
    let aborted = || {
        external_aborted.is_some_and(|f| f())
            || deadline.is_some_and(|d| std::time::Instant::now() >= d)
    };
    match measure_runtime_size(runtime_dir, Some(&aborted)) {
        Ok(size) => RuntimeSizeBudgetOutcome::Size(size),
        Err(WalkAborted) => RuntimeSizeBudgetOutcome::Unknown(RuntimeSizeUnknown {
            budget_ms: budget_ms.unwrap_or(0),
            at: format_rfc3339_millis(now_ms),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_bytes_and_stands_at_eighty_percent() {
        assert_eq!(format_runtime_bytes(500), "500 B");
        assert_eq!(format_runtime_bytes(1536), "1.5 KiB");
        assert_eq!(format_runtime_bytes(150 * 1024), "150 KiB");
        assert_eq!(
            runtime_size_standing(800, 1000),
            RuntimeSizeStanding {
                level: RuntimeSizeStandingLevel::Warn,
                percent: Some(80),
            }
        );
        assert_eq!(
            runtime_size_standing(799, 1000),
            RuntimeSizeStanding {
                level: RuntimeSizeStandingLevel::Ok,
                percent: Some(79),
            }
        );
    }
}
