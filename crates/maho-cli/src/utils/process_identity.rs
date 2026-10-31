//! Port of senpi `utils/process-identity.ts` (pin 903b8336): the pidfile identity layer
//! (#3167) shared by Native RPC and terminal lifecycle.
//!
//! This file ports the pure half: the start-time tolerance, the identity parser, the pidfile
//! parser and `processMatchesPidFile`. The live probes (start time, liveness) are injected.
//! `maho_rpc::host_reservations` already provides them for the Rust host registry, which records
//! Linux clock ticks rather than senpi's lstart, ISO or FILETIME strings.
//!
//! senpi's retry loop awaits `setTimeout(delayMs)`. Here the pause is an injected callback, so
//! callers choose the clock and tests never sleep.

use std::fmt;

use chrono::{Local, NaiveDate, NaiveDateTime, TimeZone};
use regex::Regex;
use std::sync::LazyLock;

const WINDOWS_FILETIME_EPOCH: i128 = 116_444_736_000_000_000;
pub const PROCESS_START_TOLERANCE_MS: f64 = 3_000.0;

static KOREAN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(\d{4})\x{B144}\s+(\d{1,2})\x{C6D4}\s+(\d{1,2})\x{C77C}\s+[\x{C6D4}\x{D654}\x{C218}\x{BAA9}\x{AE08}\x{D1A0}\x{C77C}]\x{C694}\x{C77C}\s+(\d{1,2})\x{C2DC}\s+(\d{1,2})\x{BD84}\s+(\d{1,2})\x{CD08}$")
        .expect("korean lstart pattern")
});
static JAPANESE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[\x{65E5}\x{6708}\x{706B}\x{6C34}\x{6728}\x{91D1}\x{571F}](?:\x{66DC}\x{65E5})?\s+(\d{1,2})/\s*(\d{1,2})\s+(\d{2}):(\d{2}):(\d{2})\s+(\d{4})$")
        .expect("japanese lstart pattern")
});

fn local_ms(naive: NaiveDateTime) -> Option<f64> {
    Local
        .from_local_datetime(&naive)
        .earliest()
        .map(|time| time.timestamp_millis() as f64)
}

fn calendar_ms(parts: [u32; 6]) -> Option<f64> {
    let [year, month, day, hour, minute, second] = parts;
    // A recognized but invalid calendar must not roll over into a neighbouring date.
    let naive = NaiveDate::from_ymd_opt(i32::try_from(year).ok()?, month, day)?
        .and_hms_opt(hour, minute, second)?;
    local_ms(naive)
}

fn captured(captures: &regex::Captures<'_>, order: [usize; 6]) -> Option<[u32; 6]> {
    let mut parts = [0; 6];
    for (slot, index) in parts.iter_mut().zip(order) {
        *slot = captures.get(index)?.as_str().parse().ok()?;
    }
    Some(parts)
}

/// senpi `processStartTimeMs`: FILETIME, C or localized `ps -o lstart` and ISO identities share
/// one millisecond representation. Persisted lstart values describe the host's local timezone.
pub fn process_start_time_ms(identity: &str) -> Option<f64> {
    if !identity.is_empty() && identity.bytes().all(|byte| byte.is_ascii_digit()) {
        let filetime: i128 = identity.parse().ok()?;
        return Some(((filetime - WINDOWS_FILETIME_EPOCH) / 10_000) as f64);
    }
    let local = identity.trim();
    if let Some(captures) = KOREAN.captures(local) {
        return calendar_ms(captured(&captures, [1, 2, 3, 4, 5, 6])?);
    }
    if let Some(captures) = JAPANESE.captures(local) {
        return calendar_ms(captured(&captures, [6, 1, 2, 3, 4, 5])?);
    }
    if let Ok(time) = chrono::DateTime::parse_from_rfc3339(local) {
        return Some(time.timestamp_millis() as f64);
    }
    // The C-locale `lstart` form: `Mon Jul  2 10:00:00 2026`. Like `Date.parse`, the weekday is
    // not checked against the date.
    let mut words = local.split_whitespace().collect::<Vec<_>>();
    if words.len() == 5 {
        words.remove(0);
    }
    NaiveDateTime::parse_from_str(&words.join(" "), "%b %d %H:%M:%S %Y")
        .ok()
        .and_then(local_ms)
}

/// senpi `sameProcessStartMs`.
pub fn same_process_start_ms(recorded: Option<f64>, observed: Option<f64>) -> bool {
    match (recorded, observed) {
        (Some(recorded), Some(observed)) => {
            recorded.is_finite()
                && observed.is_finite()
                && (recorded - observed).abs() <= PROCESS_START_TOLERANCE_MS
        }
        _ => false,
    }
}

/// senpi `DaemonPidFile`. `process_start_time: None` is senpi's `null`: the host registered while
/// its identity probe was starved, so the pid is known but its ownership cannot be proven.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DaemonPidFile {
    pub pid: u32,
    pub process_start_time: Option<String>,
}

/// senpi `parseDaemonPidFile`.
pub fn parse_daemon_pid_file(text: &str) -> Option<DaemonPidFile> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let record = value.as_object()?;
    let pid = record.get("pid")?.as_f64()?;
    if pid.fract() != 0.0 || pid <= 0.0 || pid > f64::from(u32::MAX) {
        return None;
    }
    let pid = pid as u32;
    match record.get("processStartTime")? {
        serde_json::Value::Null => Some(DaemonPidFile {
            pid,
            process_start_time: None,
        }),
        serde_json::Value::String(identity) if !identity.trim().is_empty() => Some(DaemonPidFile {
            pid,
            process_start_time: Some(identity.clone()),
        }),
        _ => None,
    }
}

/// senpi `ProcessIdentityUnreadableError`: a live process refused to yield an identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessIdentityUnreadableError {
    pub pid: u32,
    pub attempts: u32,
    pub cause: String,
}

impl fmt::Display for ProcessIdentityUnreadableError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "process identity for live pid {} stayed unreadable after {} probe attempt(s): {}",
            self.pid, self.attempts, self.cause
        )
    }
}

impl std::error::Error for ProcessIdentityUnreadableError {}

/// senpi `IdentityProbeRetry` (default 5 attempts). The pause between attempts is the caller's.
pub struct IdentityProbeRetry<'a> {
    pub attempts: u32,
    pub pause: &'a mut dyn FnMut(),
}

/// senpi `processMatchesPidFile`: whether the pidfile still describes the running process. A
/// probe failure on a pid that is gone means "gone". On a live pid it is retried, and only then
/// surfaces as [`ProcessIdentityUnreadableError`].
pub fn process_matches_pid_file(
    pid_file: &DaemonPidFile,
    read_start_time: &mut dyn FnMut(u32) -> Result<Option<String>, String>,
    is_live: &mut dyn FnMut(u32) -> bool,
    retry: IdentityProbeRetry<'_>,
) -> Result<bool, ProcessIdentityUnreadableError> {
    let Some(recorded_identity) = &pid_file.process_start_time else {
        if !is_live(pid_file.pid) {
            return Ok(false);
        }
        return Err(ProcessIdentityUnreadableError {
            pid: pid_file.pid,
            attempts: 0,
            cause: "pidfile carries no process identity guard".to_owned(),
        });
    };
    let attempts = retry.attempts.max(1);
    let mut last_error = String::new();
    for attempt in 1..=attempts {
        match read_start_time(pid_file.pid) {
            Ok(current) => {
                if let Some(current) = current {
                    let recorded = process_start_time_ms(recorded_identity);
                    let observed = process_start_time_ms(&current);
                    if recorded.is_some() && observed.is_some() {
                        return Ok(same_process_start_ms(recorded, observed));
                    }
                }
                if !is_live(pid_file.pid) {
                    return Ok(false);
                }
                last_error =
                    "process identity probe returned no usable identity for a live process"
                        .to_owned();
            }
            Err(error) => {
                if !is_live(pid_file.pid) {
                    return Ok(false);
                }
                last_error = error;
            }
        }
        if attempt < attempts {
            (retry.pause)();
        }
    }
    Err(ProcessIdentityUnreadableError {
        pid: pid_file.pid,
        attempts,
        cause: last_error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(
        pid_file: &DaemonPidFile,
        mut read: impl FnMut(u32) -> Result<Option<String>, String>,
        live: bool,
    ) -> (Result<bool, ProcessIdentityUnreadableError>, u32) {
        let mut pauses = 0;
        let mut pause = || pauses += 1;
        let result = process_matches_pid_file(
            pid_file,
            &mut read,
            &mut |_| live,
            IdentityProbeRetry {
                attempts: 5,
                pause: &mut pause,
            },
        );
        (result, pauses)
    }

    #[test]
    fn rejects_malformed_pidfiles_and_detects_start_time_mismatches() {
        assert!(parse_daemon_pid_file("{").is_none());
        let valid =
            parse_daemon_pid_file(r#"{"pid":123,"processStartTime":"Mon Jul  2 10:00:00 2026"}"#)
                .expect("valid pidfile");
        let same = matches(
            &valid,
            |_| Ok(Some("Mon Jul  2 10:00:00 2026".into())),
            true,
        )
        .0;
        let stale = matches(
            &valid,
            |_| Ok(Some("Mon Jul  2 10:00:04 2026".into())),
            true,
        )
        .0;
        assert_eq!(same, Ok(true));
        assert_eq!(stale, Ok(false));
    }

    #[test]
    fn pidfile_parser_follows_the_upstream_shape_rules() {
        assert_eq!(
            parse_daemon_pid_file(r#"{"pid":7,"processStartTime":null}"#),
            Some(DaemonPidFile {
                pid: 7,
                process_start_time: None
            })
        );
        for text in [
            r#"{"pid":0,"processStartTime":"x"}"#,
            r#"{"pid":1.5,"processStartTime":"x"}"#,
            r#"{"pid":"7","processStartTime":"x"}"#,
            r#"{"pid":7,"processStartTime":"  "}"#,
            r#"{"pid":7}"#,
            "null",
        ] {
            assert!(parse_daemon_pid_file(text).is_none(), "{text}");
        }
    }

    #[test]
    fn identity_formats_share_one_millisecond_scale() {
        assert_eq!(process_start_time_ms("116444736000000000"), Some(0.0));
        assert_eq!(
            process_start_time_ms("2026-07-02T10:00:00Z"),
            Some(1_782_986_400_000.0)
        );
        let c_locale = process_start_time_ms("Thu Jul  2 10:00:00 2026").expect("lstart");
        let korean = process_start_time_ms("2026\u{b144} 7\u{c6d4} 2\u{c77c} \u{baa9}\u{c694}\u{c77c} 10\u{c2dc} 0\u{bd84} 0\u{cd08}")
            .expect("korean lstart");
        let japanese =
            process_start_time_ms("\u{6728} 7/ 2 10:00:00 2026").expect("japanese lstart");
        assert_eq!(c_locale, korean);
        assert_eq!(c_locale, japanese);
        assert!(process_start_time_ms("2026\u{b144} 2\u{c6d4} 30\u{c77c} \u{baa9}\u{c694}\u{c77c} 10\u{c2dc} 0\u{bd84} 0\u{cd08}").is_none());
        assert!(!same_process_start_ms(None, Some(1.0)));
        assert!(same_process_start_ms(Some(0.0), Some(3_000.0)));
        assert!(!same_process_start_ms(Some(0.0), Some(3_001.0)));
    }

    #[test]
    fn probe_failures_mean_gone_on_a_dead_pid_and_retry_on_a_live_one() {
        let guarded = DaemonPidFile {
            pid: 42,
            process_start_time: Some("Thu Jul  2 10:00:00 2026".into()),
        };
        assert_eq!(
            matches(&guarded, |_| Err("starved".into()), false).0,
            Ok(false)
        );

        let mut calls = 0;
        let (result, pauses) = matches(
            &guarded,
            |_| {
                calls += 1;
                if calls == 1 {
                    Err("temporarily unavailable".into())
                } else {
                    Ok(Some("Thu Jul  2 10:00:01 2026".into()))
                }
            },
            true,
        );
        assert_eq!(result, Ok(true));
        assert_eq!(pauses, 1);

        let (result, pauses) = matches(&guarded, |_| Err("starved".into()), true);
        let error = result.expect_err("unreadable");
        assert_eq!(
            (error.attempts, error.cause.as_str(), pauses),
            (5, "starved", 4)
        );
    }

    #[test]
    fn an_unguarded_record_is_never_claimed_while_its_pid_lives() {
        let unguarded = DaemonPidFile {
            pid: 9,
            process_start_time: None,
        };
        assert_eq!(matches(&unguarded, |_| Ok(None), false).0, Ok(false));
        let error = matches(&unguarded, |_| Ok(None), true)
            .0
            .expect_err("unknown");
        assert_eq!(error.attempts, 0);
        assert_eq!(error.cause, "pidfile carries no process identity guard");
    }
}
