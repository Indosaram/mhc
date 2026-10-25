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
    let parts: Vec<&str> = text.split_whitespace().collect();
    let [_weekday, month, day, clock, year] = parts.as_slice() else {
        return None;
    };
    let month = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ]
    .iter()
    .position(|name| name == month)? as i64
        + 1;
    let day: i64 = day.parse().ok()?;
    let year: i64 = year.parse().ok()?;
    let mut clock = clock.split(':');
    let hour: i64 = clock.next()?.parse().ok()?;
    let minute: i64 = clock.next()?.parse().ok()?;
    let second: i64 = clock.next()?.parse().ok()?;
    if clock.next().is_some()
        || !(1..=31).contains(&day)
        || !(0..=23).contains(&hour)
        || !(0..=59).contains(&minute)
        || !(0..=60).contains(&second)
    {
        return None;
    }
    // Days from civil (Howard Hinnant).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hour * 3_600 + minute * 60 + second)
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
        for invalid in [
            "",
            "Sat Oct 18 05:00:00",
            "Xyz Oct 18 05:00:00 2026",
            "Sat Oct 18 25:00:00 2026",
            "Sat Oct 18 05:00 2026",
            "Sat Oct 32 05:00:00 2026",
        ] {
            assert_eq!(parse_lstart(invalid), None, "{invalid:?}");
        }
    }
}
