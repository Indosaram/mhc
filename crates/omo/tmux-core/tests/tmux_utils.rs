//! Port of src/tmux-utils.test.ts

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use common::env;
use tmux_core::{
    FetchFn, IsServerRunningOptions, ServerHealthState, TmuxLayout,
    create_server_health_state_for_testing, is_inside_tmux_environment,
    is_server_marked_running_in_process, is_server_running, is_tmux_pane_compatible_environment,
    mark_server_running_in_process, reset_server_check,
};

fn fetch_recorder(status: Result<u16, String>) -> (FetchFn, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let fetch: FetchFn = Arc::new(move |_, _| {
        counter.fetch_add(1, Ordering::SeqCst);
        status.clone()
    });
    (fetch, calls)
}

fn check(url: &str, fetch: &FetchFn, state: &mut ServerHealthState) -> bool {
    is_server_running(
        url,
        &mut IsServerRunningOptions {
            fetch_implementation: Some(Arc::clone(fetch)),
            state: Some(state),
        },
    )
}

// describe("isInsideTmux")

#[test]
fn is_inside_tmux_returns_true_when_tmux_env_is_set() {
    assert!(is_inside_tmux_environment(&*env(&[(
        "TMUX",
        "/tmp/tmux-1000/default"
    )])));
}

#[test]
fn is_inside_tmux_returns_false_when_only_cmux_socket_path_is_set() {
    assert!(!is_inside_tmux_environment(&*env(&[(
        "CMUX_SOCKET_PATH",
        "/tmp/cmux.sock"
    )])));
}

#[test]
fn pane_compatibility_returns_true_when_cmux_socket_path_is_set_without_tmux() {
    let environment = env(&[("CMUX_SOCKET_PATH", "/tmp/cmux.sock"), ("TMUX_PANE", "%0")]);
    assert!(is_tmux_pane_compatible_environment(&*environment));
}

#[test]
fn is_inside_tmux_returns_false_when_tmux_env_is_not_set() {
    assert!(!is_inside_tmux_environment(&*env(&[])));
}

#[test]
fn is_inside_tmux_returns_false_when_tmux_env_is_empty_string() {
    assert!(!is_inside_tmux_environment(&*env(&[("TMUX", "")])));
}

// "is exported as a function": N/A shape test; the functions above are the export.

// describe("isServerRunning")

#[test]
fn is_server_running_returns_true_when_server_responds_ok() {
    let mut state = create_server_health_state_for_testing();
    let (fetch, _) = fetch_recorder(Ok(200));
    assert!(check("http://localhost:4096", &fetch, &mut state));
}

#[test]
fn is_server_running_returns_false_when_server_not_reachable() {
    let mut state = create_server_health_state_for_testing();
    let (fetch, _) = fetch_recorder(Err("ECONNREFUSED".into()));
    assert!(!check("http://localhost:4096", &fetch, &mut state));
}

#[test]
fn is_server_running_returns_false_when_fetch_returns_not_ok() {
    let mut state = create_server_health_state_for_testing();
    let (fetch, _) = fetch_recorder(Ok(500));
    assert!(!check("http://localhost:4096", &fetch, &mut state));
}

#[test]
fn is_server_running_caches_successful_result() {
    let mut state = create_server_health_state_for_testing();
    let (fetch, calls) = fetch_recorder(Ok(200));
    check("http://localhost:4096", &fetch, &mut state);
    check("http://localhost:4096", &fetch, &mut state);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn is_server_running_does_not_cache_failed_result() {
    let mut state = create_server_health_state_for_testing();
    let (fetch, calls) = fetch_recorder(Err("ECONNREFUSED".into()));
    check("http://localhost:4096", &fetch, &mut state);
    check("http://localhost:4096", &fetch, &mut state);
    // 2 attempts per call, 2 calls
    assert_eq!(calls.load(Ordering::SeqCst), 4);
}

#[test]
fn is_server_running_uses_different_cache_for_different_urls() {
    let mut state = create_server_health_state_for_testing();
    let (fetch, calls) = fetch_recorder(Ok(200));
    check("http://localhost:4096", &fetch, &mut state);
    check("http://localhost:5000", &fetch, &mut state);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

// describe("resetServerCheck")

#[test]
fn reset_server_check_clears_cache_without_panicking() {
    reset_server_check();
}

#[test]
fn reset_server_check_allows_re_checking_after_reset() {
    let mut state = create_server_health_state_for_testing();
    let (fetch, calls) = fetch_recorder(Ok(200));
    check("http://localhost:4096", &fetch, &mut state);
    state.server_available = None;
    state.server_check_url = None;
    check("http://localhost:4096", &fetch, &mut state);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

// describe("markServerRunningInProcess")

#[test]
fn mark_server_running_skips_http_fetch_when_marked_running_in_process() {
    let mut state = create_server_health_state_for_testing();
    state.server_running_in_process = true;
    let (fetch, calls) = fetch_recorder(Ok(200));
    assert!(check("http://localhost:4096", &fetch, &mut state));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn mark_server_running_sets_process_global_flag() {
    reset_server_check();
    mark_server_running_in_process();
    assert!(is_server_marked_running_in_process());
    reset_server_check();
    assert!(!is_server_marked_running_in_process());
}

// describe("tmux pane functions"): "exported as function" checks are compile-time here.
#[test]
fn tmux_pane_functions_are_exported() {
    let _spawn: fn(
        &tmux_core::SpawnTmuxPaneRequest<'_>,
        &tmux_core::TmuxDeps,
    ) -> tmux_core::SpawnPaneResult = tmux_core::spawn_tmux_pane;
    let _close: fn(&str) -> bool = tmux_core::close_tmux_pane;
    let _layout: fn(&str, TmuxLayout, u32, &tmux_core::LayoutDeps) = tmux_core::apply_layout;
}
