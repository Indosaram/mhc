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
    #[cfg(target_os = "linux")]
    {
        let value = identity.expect("Linux start identity should resolve for current process");
        assert!(value.starts_with("linux-proc-start-ticks:"));
    }

    #[cfg(any(target_os = "macos", target_os = "freebsd"))]
    {
        let value = identity.expect("the preserved ps probe resolves the current process");
        assert!(value.starts_with("ps-lstart:"));
        let remainder = &value["ps-lstart:".len()..];
        assert!(!remainder.contains("  "));
    }

    #[cfg(target_os = "windows")]
    {
        let value = identity.expect("GetProcessTimes resolves the current process");
        assert!(value.starts_with("windows-process-creation-filetime:"));
    }
}

#[test]
fn given_the_own_pid_when_start_identity_is_queried_twice_then_the_value_is_stable() {
    let current_pid = std::process::id();
    let first = get_process_start_identity(current_pid);
    let second = get_process_start_identity(current_pid);
    assert_eq!(first, second);
}

#[test]
fn incompatible_identity_schemes_do_not_conflict_or_prove_comparability() {
    assert!(!start_identities_conflict(
        "ps-lstart:date",
        "proc-start-epoch:123"
    ));
    assert!(!start_identities_comparable(
        "ps-lstart:date",
        "proc-start-epoch:123"
    ));
}

/// The Linux branch is the fork-free port: `/proc` is read in process, no child is ever spawned.
#[cfg(target_os = "linux")]
#[test]
fn start_identity_never_forks_a_child_on_linux() {
    let source = include_str!("process_identity.rs");
    let identity_section = source
        .split("pub fn get_process_start_identity")
        .nth(1)
        .expect("start-identity reader present");
    let readers = identity_section
        .split("pub fn get_pid_liveness")
        .next()
        .expect("liveness reader follows");
    assert!(
        !readers.contains("Command::new"),
        "the Linux start-identity path must not spawn a child process"
    );
}

/// macOS keeps the pre-existing `/bin/ps` probe: the pinned `libproc` replacement is FFI and this
/// crate forbids `unsafe_code`, so the fork-free port is a recorded platform blocker.
#[cfg(any(target_os = "macos", target_os = "freebsd"))]
#[test]
fn bsd_start_identity_preserves_the_ps_probe_until_a_safe_binding_exists() {
    let value = get_process_start_identity(std::process::id())
        .expect("the preserved ps probe resolves the current process");
    assert!(value.starts_with("ps-lstart:"));
}

/// Windows resolves the creation FILETIME in process via `GetProcessTimes`.
#[cfg(target_os = "windows")]
#[test]
fn windows_start_identity_resolves_the_creation_filetime() {
    let value = get_process_start_identity(std::process::id())
        .expect("GetProcessTimes resolves the current process");
    assert!(value.starts_with("windows-process-creation-filetime:"));
}

#[cfg(target_os = "linux")]
#[test]
fn linux_start_identity_is_an_in_process_proc_read() {
    let identity = get_process_start_identity(std::process::id()).expect("linux token");
    let ticks = identity
        .strip_prefix("linux-proc-start-ticks:")
        .expect("token prefix");
    assert!(!ticks.is_empty() && ticks.bytes().all(|byte| byte.is_ascii_digit()));
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

#[test]
fn given_current_process_when_start_identity_queried_twice_then_cached_value_matches_fresh_probe() {
    // #given
    let current_pid = std::process::id();

    // #when
    let first = get_process_start_identity(current_pid);
    let second = get_process_start_identity(current_pid);

    // #then
    assert_eq!(first, second);
    assert_eq!(first, probe_process_start_identity(current_pid));
}
