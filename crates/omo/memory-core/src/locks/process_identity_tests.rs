use pretty_assertions::assert_eq;

use super::*;

#[test]
fn given_current_process_when_liveness_probed_then_reports_alive() {
    // #given
    let current_pid = std::process::id();

    // #when
    let liveness = get_pid_liveness(current_pid);

    // #then
    assert_eq!(liveness, ProcessLiveness::Alive);
}

#[test]
fn given_non_existent_pid_when_liveness_probed_then_reports_dead() {
    // #given
    let dead_pid = 2_000_000_000;

    // #when
    let liveness = get_pid_liveness(dead_pid);

    // #then
    assert_eq!(liveness, ProcessLiveness::Dead);
}

#[test]
fn given_current_process_when_start_identity_queried_then_format_matches() {
    // #given
    let current_pid = std::process::id();

    // #when
    let identity = get_process_start_identity(current_pid);

    // #then
    #[cfg(any(target_os = "macos", target_os = "freebsd"))]
    {
        let value = identity.expect("BSD start identity should resolve for current process");
        assert!(value.starts_with("ps-lstart:"));
        let remainder = &value["ps-lstart:".len()..];
        assert!(!remainder.contains("  "));
    }

    #[cfg(target_os = "linux")]
    {
        let value = identity.expect("Linux start identity should resolve for current process");
        assert!(value.starts_with("linux-proc-start-ticks:"));
    }
}

#[test]
fn given_non_existent_pid_when_start_identity_queried_then_returns_none() {
    // #given
    let dead_pid = 2_000_000_000;

    // #when
    let identity = get_process_start_identity(dead_pid);

    // #then
    assert_eq!(identity, None);
}
