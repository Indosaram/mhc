//! Port of `config/memory-settings.ts` (senpi 903b8336): the `memory` settings object, its
//! defaults, file validation and environment overrides.

use super::settings::{CodemodeSettings, Environment};
use serde::{Deserialize, Serialize};

/// The longest delay setTimeout honours (2^31 - 1 ms); kept as the upstream bound.
pub const MAX_IDLE_PARK_MINUTES: f64 = 35_791.0;
pub const DEFAULT_MEMORY_GC_WATERMARK_MB: f64 = 256.0;
pub const DEFAULT_MEMORY_NOTICE_MB: f64 = 1024.0;
pub const DEFAULT_RETAINED_RESULTS_MB: u64 = 32;
pub const DEFAULT_RETAINED_IMAGES_MB: u64 = 256;
const CEILING_FLOOR_MB: f64 = 2048.0;
const CEILING_CAP_MB: f64 = 8192.0;

pub const MEMORY_GC_WATERMARK_ENVIRONMENT_FLAG: &str = "SENPI_CODEMODE_MEMORY_GC_WATERMARK_MB";
pub const MEMORY_NOTICE_ENVIRONMENT_FLAG: &str = "SENPI_CODEMODE_MEMORY_NOTICE_MB";
pub const MEMORY_CEILING_ENVIRONMENT_FLAG: &str = "SENPI_CODEMODE_MEMORY_CEILING_MB";
pub const RETAINED_RESULTS_ENVIRONMENT_FLAG: &str = "SENPI_CODEMODE_RETAINED_RESULTS_MB";
pub const RETAINED_IMAGES_ENVIRONMENT_FLAG: &str = "SENPI_CODEMODE_RETAINED_IMAGES_MB";

const MIB: f64 = 1024.0 * 1024.0;

/// The `memory` object as written in codemode.json (every key optional, no other keys).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CodemodeMemorySettingsInput {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gc_watermark_mb: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notice_mb: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ceiling_mb: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retained_results_mb: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retained_images_mb: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idle_park_minutes: Option<f64>,
}

impl CodemodeMemorySettingsInput {
    /// memorySettingsSchema: every number is >= 0, and idleParkMinutes is at most MAX_IDLE_PARK_MINUTES.
    pub fn valid(&self) -> bool {
        let non_negative = [
            self.gc_watermark_mb,
            self.notice_mb,
            self.ceiling_mb,
            self.retained_results_mb,
            self.retained_images_mb,
            self.idle_park_minutes,
        ]
        .into_iter()
        .flatten()
        .all(|value| value.is_finite() && value >= 0.0);
        non_negative
            && self
                .idle_park_minutes
                .is_none_or(|minutes| minutes <= MAX_IDLE_PARK_MINUTES)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CodemodeMemorySettings {
    /// Live kernel memory from which a finished cell triggers a full collection.
    pub gc_watermark_mb: f64,
    /// Live kernel memory after collection from which a result names the largest globals.
    pub notice_mb: f64,
    /// Live kernel memory after collection from which the kernel restarts once its queue drains.
    pub ceiling_mb: f64,
    /// In-memory byte budget (MiB) for settled-cell snapshots kept for peek/list; 0 keeps only the count cap.
    pub retained_results_mb: f64,
    /// Disk budget (MiB) for settled-cell images spilled under the session artifacts dir; 0 keeps only the count cap.
    pub retained_images_mb: f64,
    /// Minutes with no cell running or queued before a kernel is closed and restarted on the next cell.
    pub idle_park_minutes: Option<f64>,
}

/// Total physical memory in bytes (`os.totalmem()`), or `None` when it cannot be read.
fn total_memory_bytes() -> Option<f64> {
    #[cfg(target_os = "linux")]
    {
        let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
        let line = meminfo.lines().find(|line| line.starts_with("MemTotal:"))?;
        let kib: f64 = line.split_whitespace().nth(1)?.parse().ok()?;
        Some(kib * 1024.0)
    }
    #[cfg(target_os = "macos")]
    {
        let output = std::process::Command::new("sysctl")
            .args(["-n", "hw.memsize"])
            .output()
            .ok()?;
        String::from_utf8_lossy(&output.stdout).trim().parse().ok()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

/// A quarter of total memory, kept within 2-8 GiB.
pub fn default_memory_ceiling_mb(total_bytes: Option<f64>) -> f64 {
    let quarter_mb = (total_bytes.or_else(total_memory_bytes).unwrap_or(0.0) / MIB / 4.0).floor();
    CEILING_CAP_MB.min(CEILING_FLOOR_MB.max(quarter_mb))
}

pub fn default_memory_settings() -> CodemodeMemorySettings {
    CodemodeMemorySettings {
        gc_watermark_mb: DEFAULT_MEMORY_GC_WATERMARK_MB,
        notice_mb: DEFAULT_MEMORY_NOTICE_MB,
        ceiling_mb: default_memory_ceiling_mb(None),
        retained_results_mb: DEFAULT_RETAINED_RESULTS_MB as f64,
        retained_images_mb: DEFAULT_RETAINED_IMAGES_MB as f64,
        idle_park_minutes: None,
    }
}

pub fn merge_memory_settings(
    input: Option<&CodemodeMemorySettingsInput>,
) -> CodemodeMemorySettings {
    let defaults = default_memory_settings();
    let Some(input) = input else {
        return defaults;
    };
    CodemodeMemorySettings {
        gc_watermark_mb: input.gc_watermark_mb.unwrap_or(defaults.gc_watermark_mb),
        notice_mb: input.notice_mb.unwrap_or(defaults.notice_mb),
        ceiling_mb: input.ceiling_mb.unwrap_or(defaults.ceiling_mb),
        retained_results_mb: input
            .retained_results_mb
            .unwrap_or(defaults.retained_results_mb),
        retained_images_mb: input
            .retained_images_mb
            .unwrap_or(defaults.retained_images_mb),
        idle_park_minutes: input.idle_park_minutes,
    }
}

pub fn memory_settings_order_warning(memory: &CodemodeMemorySettings) -> Option<String> {
    let enabled: Vec<f64> = [memory.gc_watermark_mb, memory.notice_mb, memory.ceiling_mb]
        .into_iter()
        .filter(|value| *value > 0.0)
        .collect();
    if enabled.windows(2).all(|pair| pair[0] <= pair[1]) {
        return None;
    }
    Some(format!(
        "Codemode memory thresholds must satisfy gcWatermarkMb <= noticeMb <= ceilingMb (got {}, {}, {}); using the defaults.",
        memory.gc_watermark_mb, memory.notice_mb, memory.ceiling_mb
    ))
}

/// File thresholds out of order fall back to the defaults with a warning; the settled-cell budgets are kept.
pub fn validated_memory_settings(
    input: Option<&CodemodeMemorySettingsInput>,
) -> (CodemodeMemorySettings, Vec<String>) {
    let merged = merge_memory_settings(input);
    let Some(warning) = memory_settings_order_warning(&merged) else {
        return (merged, vec![]);
    };
    let defaults = default_memory_settings();
    (
        CodemodeMemorySettings {
            retained_results_mb: merged.retained_results_mb,
            retained_images_mb: merged.retained_images_mb,
            idle_park_minutes: merged.idle_park_minutes,
            ..defaults
        },
        vec![warning],
    )
}

fn megabytes_override(value: Option<&String>) -> Option<f64> {
    let value = value?;
    if value.trim().is_empty() {
        return None;
    }
    let parsed: f64 = value.trim().parse().ok()?;
    (parsed.is_finite() && parsed >= 0.0).then_some(parsed)
}

fn non_negative_integer_override(value: Option<&String>) -> Option<u64> {
    let value = value?.trim();
    (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| value.parse().ok())
        .flatten()
}

/// Environment overrides win over the settings file; `0` disables a threshold, and a malformed value is
/// ignored. Thresholds out of order after the overrides fall back to the defaults.
pub fn resolve_kernel_memory_thresholds(
    memory: Option<&CodemodeMemorySettings>,
    env: &Environment,
) -> crate::bridge::protocol::KernelMemoryThresholds {
    let base = memory.cloned().unwrap_or_else(default_memory_settings);
    let resolved = CodemodeMemorySettings {
        gc_watermark_mb: megabytes_override(env.get(MEMORY_GC_WATERMARK_ENVIRONMENT_FLAG))
            .unwrap_or(base.gc_watermark_mb),
        notice_mb: megabytes_override(env.get(MEMORY_NOTICE_ENVIRONMENT_FLAG))
            .unwrap_or(base.notice_mb),
        ceiling_mb: megabytes_override(env.get(MEMORY_CEILING_ENVIRONMENT_FLAG))
            .unwrap_or(base.ceiling_mb),
        ..base
    };
    let effective = if memory_settings_order_warning(&resolved).is_none() {
        resolved
    } else {
        default_memory_settings()
    };
    crate::bridge::protocol::KernelMemoryThresholds {
        gc_watermark_bytes: (effective.gc_watermark_mb * MIB).round() as u64,
        notice_bytes: (effective.notice_mb * MIB).round() as u64,
        ceiling_bytes: (effective.ceiling_mb * MIB).round() as u64,
    }
}

/// Settled-cell in-memory snapshot byte budget; the environment override accepts 0 (count cap only).
pub fn resolve_retained_results_bytes(settings: &CodemodeSettings, env: &Environment) -> u64 {
    let megabytes = non_negative_integer_override(env.get(RETAINED_RESULTS_ENVIRONMENT_FLAG))
        .map(|value| value as f64)
        .unwrap_or(settings.memory().retained_results_mb);
    (megabytes * MIB) as u64
}

/// Settled-cell image spill disk budget; the environment override accepts 0 (count cap only).
pub fn resolve_retained_images_bytes(settings: &CodemodeSettings, env: &Environment) -> u64 {
    let megabytes = non_negative_integer_override(env.get(RETAINED_IMAGES_ENVIRONMENT_FLAG))
        .map(|value| value as f64)
        .unwrap_or(settings.memory().retained_images_mb);
    (megabytes * MIB) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: f64 = 1024.0 * MIB;

    #[test]
    fn ceiling_defaults_to_a_quarter_of_memory_within_two_to_eight_gib() {
        for (total, ceiling) in [
            (4.0 * GIB, 2048.0),
            (16.0 * GIB, 4096.0),
            (128.0 * GIB, 8192.0),
        ] {
            assert_eq!(default_memory_ceiling_mb(Some(total)), ceiling);
        }
    }

    #[test]
    fn environment_overrides_win_zero_disables_and_disorder_falls_back() {
        let file = merge_memory_settings(Some(&CodemodeMemorySettingsInput {
            gc_watermark_mb: Some(100.0),
            notice_mb: Some(500.0),
            ceiling_mb: Some(3000.0),
            ..Default::default()
        }));
        let env = |pairs: &[(&str, &str)]| -> Environment {
            pairs
                .iter()
                .map(|(key, value)| ((*key).into(), (*value).into()))
                .collect()
        };
        let from_file = resolve_kernel_memory_thresholds(Some(&file), &env(&[]));
        assert_eq!(
            (
                from_file.gc_watermark_bytes,
                from_file.notice_bytes,
                from_file.ceiling_bytes
            ),
            (100 * 1_048_576, 500 * 1_048_576, 3000 * 1_048_576)
        );
        let overridden = resolve_kernel_memory_thresholds(
            Some(&file),
            &env(&[
                (MEMORY_NOTICE_ENVIRONMENT_FLAG, "0"),
                (MEMORY_CEILING_ENVIRONMENT_FLAG, "1024"),
                (MEMORY_GC_WATERMARK_ENVIRONMENT_FLAG, "bad"),
            ]),
        );
        assert_eq!(
            (
                overridden.gc_watermark_bytes,
                overridden.notice_bytes,
                overridden.ceiling_bytes
            ),
            (100 * 1_048_576, 0, 1024 * 1_048_576)
        );
        let disordered = resolve_kernel_memory_thresholds(
            None,
            &env(&[(MEMORY_GC_WATERMARK_ENVIRONMENT_FLAG, "2048")]),
        );
        assert_eq!(disordered.gc_watermark_bytes, 256 * 1_048_576);
    }

    #[test]
    fn idle_park_bound_and_negative_values_are_invalid() {
        let input = |minutes: f64| CodemodeMemorySettingsInput {
            idle_park_minutes: Some(minutes),
            ..Default::default()
        };
        assert!(input(35_791.0).valid());
        assert!(!input(35_792.0).valid());
        assert!(!input(-1.0).valid());
        let (settings, warnings) = validated_memory_settings(Some(&CodemodeMemorySettingsInput {
            notice_mb: Some(4096.0),
            ceiling_mb: Some(2048.0),
            ..Default::default()
        }));
        assert_eq!(warnings.len(), 1);
        assert_eq!(settings, default_memory_settings());
    }
}
