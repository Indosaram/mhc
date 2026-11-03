//! Port of senpi `terminal/process-identity.ts`: boot and own-process start instants used to
//! tell a live lease holder or watcher apart from a recycled pid.
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

pub use maho_tool_child::group_ownership::same_process_start_ms as same_process_start;

/// Upstream re-exports `PROCESS_START_TOLERANCE_MS` from `utils/process-identity.ts` (3 s).
pub const PROCESS_START_TOLERANCE_MS: f64 = 3_000.0;

/// Two boot instants further apart than this belong to different boots.
pub const BOOT_INSTANT_TOLERANCE_MS: f64 = 120_000.0;

fn now_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |elapsed| elapsed.as_secs_f64() * 1000.0)
}

fn whole_seconds(ms: f64) -> f64 {
    (ms / 1000.0).round() * 1000.0
}

fn floor_to_second(ms: f64) -> f64 {
    (ms / 1000.0).floor() * 1000.0
}

/// `os.uptime()` in integral seconds, as Node reports it.
fn os_uptime_seconds() -> f64 {
    #[cfg(target_os = "linux")]
    {
        if let Some(seconds) = std::fs::read_to_string("/proc/uptime")
            .ok()
            .and_then(|text| text.split_whitespace().next()?.parse::<f64>().ok())
        {
            return seconds.floor();
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(boot) = std::process::Command::new("sysctl")
            .args(["-n", "kern.boottime"])
            .output()
            .ok()
            .and_then(|out| {
                let text = String::from_utf8_lossy(&out.stdout).into_owned();
                let start = text.find("sec = ")? + 6;
                text[start..]
                    .split(|c: char| !c.is_ascii_digit())
                    .next()?
                    .parse::<f64>()
                    .ok()
            })
        {
            return (now_ms() / 1000.0 - boot).floor();
        }
    }
    platform_uptime_seconds()
}

/// libuv's `uv_uptime` on Windows: whole seconds since boot from GetTickCount64.
#[cfg(windows)]
fn platform_uptime_seconds() -> f64 {
    (winsafe::GetTickCount64() / 1000) as f64
}

#[cfg(not(windows))]
fn platform_uptime_seconds() -> f64 {
    0.0
}

/// `processBootAtMs`: the boot instant rounded to whole seconds.
pub fn process_boot_at_ms(now: Option<&dyn Fn() -> f64>) -> f64 {
    let now = now.map_or_else(now_ms, |now| now());
    whole_seconds(now - os_uptime_seconds() * 1000.0)
}

/// `process.uptime()` in ms. Linux derives it from `/proc/self/stat` start ticks (USER_HZ 100,
/// as `group_ownership::process_start_time_ms` assumes); elsewhere the origin is this module's
/// first use, the native analogue of a worker isolate's uptime origin.
fn process_uptime_ms() -> f64 {
    #[cfg(target_os = "linux")]
    {
        let start_ticks = std::fs::read_to_string("/proc/self/stat")
            .ok()
            .and_then(|stat| {
                let (_, fields) = stat.rsplit_once(") ")?;
                fields.split_whitespace().nth(19)?.parse::<f64>().ok()
            });
        let uptime = std::fs::read_to_string("/proc/uptime")
            .ok()
            .and_then(|text| text.split_whitespace().next()?.parse::<f64>().ok());
        if let (Some(ticks), Some(uptime)) = (start_ticks, uptime) {
            return (uptime - ticks / 100.0).max(0.0) * 1000.0;
        }
    }
    static FIRST_USE: OnceLock<std::time::Instant> = OnceLock::new();
    FIRST_USE
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_secs_f64()
        * 1000.0
}

static OWN_START_AT_LOAD: OnceLock<f64> = OnceLock::new();

/// `ownProcessStartedAtMs`: captured once (a monotonic uptime stops during a Linux suspend), and
/// floored like `ps -o lstart` so it never lands after now.
pub fn own_process_started_at_ms(now: Option<&dyn Fn() -> f64>) -> f64 {
    match now {
        None => *OWN_START_AT_LOAD.get_or_init(|| floor_to_second(now_ms() - process_uptime_ms())),
        Some(now) => floor_to_second(now() - process_uptime_ms()),
    }
}

pub fn same_boot(boot_at_ms: f64, other_boot_at_ms: Option<f64>) -> bool {
    let Some(other) = other_boot_at_ms.filter(|value| value.is_finite()) else {
        return false;
    };
    (boot_at_ms - other).abs() <= BOOT_INSTANT_TOLERANCE_MS
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChildProcessIdentity {
    pub pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_group_id: Option<u32>,
    pub started_at_ms: f64,
    pub boot_at_ms: f64,
    pub argv: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_a_whole_second_boot_instant_that_two_consecutive_reads_agree_on() {
        let boot = process_boot_at_ms(None);
        assert_eq!(boot % 1000.0, 0.0);
        assert!(boot < now_ms());
        assert!((process_boot_at_ms(None) - boot).abs() <= 1000.0);
    }

    #[test]
    fn reports_the_own_process_start_within_a_few_minutes_of_now() {
        let started = own_process_started_at_ms(None);
        assert_eq!(started % 1000.0, 0.0);
        assert!(started <= now_ms());
        assert!(now_ms() - started < 15.0 * 60_000.0);
    }

    #[test]
    fn floors_the_own_start_to_the_second_so_it_never_lands_after_now() {
        let fixed = 1_790_000_000_600.0 + process_uptime_ms();
        let started = {
            let clock = || fixed;
            own_process_started_at_ms(Some(&clock))
        };
        assert_eq!(started % 1000.0, 0.0);
        assert!(started <= fixed);
    }

    #[test]
    fn treats_two_boot_instants_as_the_same_boot_only_inside_the_tolerance() {
        let boot = 1_700_000_000_000.0;
        assert!(same_boot(boot, Some(boot + BOOT_INSTANT_TOLERANCE_MS)));
        assert!(same_boot(boot, Some(boot - BOOT_INSTANT_TOLERANCE_MS)));
        assert!(!same_boot(
            boot,
            Some(boot + BOOT_INSTANT_TOLERANCE_MS + 1.0)
        ));
        assert!(!same_boot(boot, None));
    }
}
