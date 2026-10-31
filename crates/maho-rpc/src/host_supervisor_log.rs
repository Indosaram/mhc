//! The supervisor's stderr lines (senpi `host-supervisor-log.ts`). A detached daemon exiting right after a
//! buffered write can lose it, so every line is written and flushed synchronously.
use std::io::Write;

/// senpi `writeStderrLine`: one line on fd 2; an unavailable fd 2 leaves nothing more to do.
pub fn write_stderr_line(text: &str) {
    let mut stderr = std::io::stderr().lock();
    let _ = writeln!(stderr, "{text}").and_then(|()| stderr.flush());
}

/// senpi `supervisorLog`.
pub fn supervisor_log(message: &str) {
    write_stderr_line(&format!("senpi rpc host supervisor: {message}"));
}

/// The line `writeHostReady` prints once startup publication completed.
pub fn host_ready_line(socket: &str, cold_start: &str, idle_exit_ms: f64) -> String {
    let idle = if cold_start == "persistent" {
        "never".to_owned()
    } else {
        format_ms(idle_exit_ms)
    };
    format!("senpi rpc host ready on unix://{socket} (coldStart={cold_start}, idleExitMs={idle})")
}

/// senpi `writeHostReady`.
pub fn write_host_ready(socket: &str, cold_start: &str, idle_exit_ms: f64) {
    write_stderr_line(&host_ready_line(socket, cold_start, idle_exit_ms));
}

/// The records an unknown-identity diagnostic names (senpi `logUnknownHostIdentity`'s `record`).
pub const UNKNOWN_IDENTITY_RECORDS: [&str; 3] =
    ["legacy_host.pid", "host.pid", "session-path-claim"];

/// Identity refusal loops share one process-local diagnostic budget, not a persisted latch: `true` the first time a
/// record and pid are seen in this process.
fn first_unknown_host_identity(record: &str, pid: u32) -> bool {
    static LOGGED: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    LOGGED
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(format!("{record}:{pid}"))
}

/// senpi `logUnknownHostIdentity`: one `{"event":"legacy_host_identity_unknown"}` line per record and pid per process.
pub fn log_unknown_host_identity(record: &str, pid: u32) {
    if first_unknown_host_identity(record, pid) {
        write_stderr_line(
            &serde_json::json!({"event": "legacy_host_identity_unknown", "record": record, "pid": pid})
                .to_string(),
        );
    }
}

/// senpi `errorMessage`: an error's message, or the value's string form.
pub fn error_message(cause: &dyn std::fmt::Display) -> String {
    cause.to_string()
}

/// JavaScript's `String(number)` for the values a policy carries (integers print without a fraction).
fn format_ms(value: f64) -> String {
    if value.is_infinite() {
        return if value > 0. { "Infinity" } else { "-Infinity" }.into();
    }
    if value.fract() == 0. && value.abs() < 1e21 {
        return format!("{value:.0}");
    }
    value.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_line_names_the_policy_and_never_for_persistent_hosts() {
        assert_eq!(
            host_ready_line("/tmp/rpc.sock", "on-demand", 900_000.),
            "senpi rpc host ready on unix:///tmp/rpc.sock (coldStart=on-demand, idleExitMs=900000)"
        );
        assert_eq!(
            host_ready_line("/tmp/rpc.sock", "persistent", 900_000.),
            "senpi rpc host ready on unix:///tmp/rpc.sock (coldStart=persistent, idleExitMs=never)"
        );
    }

    /// senpi issue-2951: repeated unknown-identity checks log once per record and pid in one process.
    #[test]
    fn an_unknown_identity_is_logged_once_per_record_and_pid() {
        let pid = u32::MAX - 7;
        for record in UNKNOWN_IDENTITY_RECORDS {
            assert!(first_unknown_host_identity(record, pid));
            assert!(!first_unknown_host_identity(record, pid));
        }
        assert!(first_unknown_host_identity("host.pid", pid - 1));
    }

    #[test]
    fn error_message_is_the_display_form() {
        assert_eq!(error_message(&std::io::Error::other("boom")), "boom");
    }
}
