//! Port of `packages/omo-senpi/src/components/telemetry/session-summary-process-start.ts`.
//!
//! When `pid`'s process started, as an opaque token compared only against tokens this same probe produced on
//! this machine, or `None` when it cannot be read. Synchronous because the owner registry is.
//!
//! Linux: field 22 of `/proc/<pid>/stat` (clock ticks since boot), a file read with no subprocess.
//! Other Unix: `ps -o lstart=` in the C locale at UTC. Windows: `None`, because its only source is the
//! process creation time behind a PowerShell or WMI call, far too slow for every liveness check.

use std::process::{Command, Stdio};

/// The platform this build reports, mirroring `process.platform`.
pub const fn current_platform() -> &'static str {
    if cfg!(target_os = "windows") {
        "win32"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else {
        "other"
    }
}

pub fn read_process_start_time(pid: u32) -> Option<String> {
    read_process_start_time_for(pid, current_platform())
}

pub fn read_process_start_time_for(pid: u32, platform: &str) -> Option<String> {
    match platform {
        "win32" => None,
        "linux" => proc_stat_start_time(pid),
        _ => ps_start_time(pid),
    }
}

fn proc_stat_start_time(pid: u32) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The command name (field 2) is parenthesized and may hold spaces or parens: fields resume after the
    // last ")".
    let tail = &stat[stat.rfind(')')? + 1..];
    let fields: Vec<&str> = tail.split(' ').collect();
    let ticks = *fields.get(20)?;
    (!ticks.is_empty() && ticks.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| format!("proc:{ticks}"))
}

fn ps_start_time(pid: u32) -> Option<String> {
    let output = Command::new("ps")
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .env("TZ", "UTC")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let seconds = parse_lstart(&String::from_utf8_lossy(&output.stdout))?;
    Some(format!("epoch:{seconds}"))
}

/// Seconds since the epoch of a `ps -o lstart=` line: `Sat Oct 18 05:00:00 2026`.
fn parse_lstart(text: &str) -> Option<i64> {
    // Match thread/gateway/process-identity.ts: weekday is a shape, not a calendar check.
    let (weekday, rest) = text.trim().split_once(' ')?;
    let weekday = weekday.as_bytes();
    if weekday.len() != 3
        || !weekday[0].is_ascii_uppercase()
        || !weekday[1..].iter().all(u8::is_ascii_lowercase)
    {
        return None;
    }
    let (month, rest) = rest.split_once(' ')?;
    let (day, rest) = rest.trim_start_matches(' ').split_once(' ')?;
    let (clock, year) = rest.split_once(' ')?;
    let digits = |value: &str, min: usize, max: usize| {
        (min..=max).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_digit())
    };
    if !digits(day, 1, 2) || !digits(year, 4, 4) {
        return None;
    }
    let month = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ]
    .iter()
    .position(|name| *name == month)? as u32
        + 1;
    let day: i64 = day.parse().ok()?;
    let year: i32 = year.parse().ok()?;
    let parts: Vec<&str> = clock.split(':').collect();
    if parts.len() != 3 || !parts.iter().all(|part| digits(part, 2, 2)) {
        return None;
    }
    let hour: i64 = parts[0].parse().ok()?;
    let minute: i64 = parts[1].parse().ok()?;
    let second: i64 = parts[2].parse().ok()?;
    // Date.UTC normalizes overflow and interprets years 00..99 as 1900..1999.
    let year = if year < 100 { year + 1900 } else { year };
    let start = chrono::NaiveDate::from_ymd_opt(year, month, 1)?.and_hms_opt(0, 0, 0)?;
    Some(start.and_utc().timestamp() + (day - 1) * 86_400 + hour * 3_600 + minute * 60 + second)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_never_reports_a_start_token() {
        assert_eq!(read_process_start_time_for(std::process::id(), "win32"), None);
    }

    #[test]
    fn linux_reads_own_start_ticks_without_a_subprocess() {
        if current_platform() != "linux" {
            return;
        }
        let token = read_process_start_time_for(std::process::id(), "linux").expect("own start ticks");
        assert!(token.starts_with("proc:"));
        assert!(token[5..].bytes().all(|byte| byte.is_ascii_digit()));
        assert_eq!(
            read_process_start_time_for(std::process::id(), "linux").as_deref(),
            Some(token.as_str())
        );
    }

    #[test]
    fn missing_pid_reads_none() {
        assert_eq!(read_process_start_time_for(u32::MAX, "linux"), None);
        assert_eq!(read_process_start_time_for(u32::MAX, "darwin"), None);
    }

    #[test]
    fn lstart_parsing_is_utc_and_strict() {
        assert_eq!(parse_lstart("Sun Oct 18 05:00:00 2026\n"), Some(1_792_299_600));
        assert_eq!(parse_lstart("  Thu Oct  8 05:00:00 2026  "), Some(1_791_435_600));
        // Upstream thread/gateway/process-identity.ts uses Date.UTC normalization.
        assert_eq!(parse_lstart("Xyz Oct 18 05:00:00 2026"), Some(1_792_299_600));
        assert_eq!(parse_lstart("Sat Oct 18 25:00:00 2026"), Some(1_792_371_600));
        assert_eq!(parse_lstart("Sat Oct 32 05:00:00 2026"), Some(1_793_509_200));
        for invalid in [
            "",
            "Sat Oct 18 05:00:00",
            "xyz Oct 18 05:00:00 2026",
            "Sat Xxx 18 05:00:00 2026",
            "Sat Oct 18 05:00 2026",
            "Sat Oct 18 5:00:00 2026",
        ] {
            assert_eq!(parse_lstart(invalid), None, "{invalid:?}");
        }
    }
}
