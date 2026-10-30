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
}
