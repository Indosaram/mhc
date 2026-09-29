//! Port of src/tmux-utils/pane-close.test.ts

mod common;

use std::sync::{Arc, Mutex};

use common::{Call, strings};
use pretty_assertions::assert_eq;
use tmux_core::{TmuxCommandResult, TmuxDeps, close_tmux_pane_with_dependencies};

struct Fixture {
    calls: Arc<Mutex<Vec<Call>>>,
    delay_calls: Arc<Mutex<Vec<u64>>>,
    dependencies: TmuxDeps,
}

/// Mirrors the TS fixture: queued results, falling back to success once drained.
fn fixture(inside_tmux: bool, tmux_path: Option<&str>, results: Vec<TmuxCommandResult>) -> Fixture {
    let calls: Arc<Mutex<Vec<Call>>> = Arc::default();
    let delay_calls: Arc<Mutex<Vec<u64>>> = Arc::default();
    let results = Arc::new(Mutex::new(results));
    let tmux_path = tmux_path.map(str::to_owned);
    let (call_sink, delay_sink) = (Arc::clone(&calls), Arc::clone(&delay_calls));
    let dependencies = TmuxDeps {
        is_inside_tmux: Some(Arc::new(move || inside_tmux)),
        get_tmux_path: Arc::new(move || tmux_path.clone()),
        run_tmux_command: Arc::new(move |tmux, args| {
            call_sink
                .lock()
                .unwrap()
                .push((tmux.to_owned(), args.to_vec()));
            let mut results = results.lock().unwrap();
            if results.is_empty() {
                TmuxCommandResult::new("", "", 0)
            } else {
                results.remove(0)
            }
        }),
        delay: Arc::new(move |milliseconds| delay_sink.lock().unwrap().push(milliseconds)),
        ..TmuxDeps::default()
    };
    Fixture {
        calls,
        delay_calls,
        dependencies,
    }
}

fn ok() -> TmuxCommandResult {
    TmuxCommandResult::new("", "", 0)
}

#[test]
fn pane_exists_returns_true_and_sends_keys_then_kills_in_order() {
    let fixture = fixture(true, Some("tmux"), vec![ok()]);
    let result = close_tmux_pane_with_dependencies("%42", &fixture.dependencies);
    assert!(result);
    assert_eq!(
        *fixture.calls.lock().unwrap(),
        vec![
            (
                "tmux".to_owned(),
                strings(&["send-keys", "-t", "%42", "C-c"])
            ),
            ("tmux".to_owned(), strings(&["kill-pane", "-t", "%42"])),
        ]
    );
    assert_eq!(*fixture.delay_calls.lock().unwrap(), vec![250]);
}

#[test]
fn not_inside_tmux_returns_false_without_runner_calls() {
    let fixture = fixture(false, Some("tmux"), vec![ok()]);
    assert!(!close_tmux_pane_with_dependencies(
        "%42",
        &fixture.dependencies
    ));
    assert!(fixture.calls.lock().unwrap().is_empty());
}

#[test]
fn tmux_not_found_returns_false_without_runner_calls() {
    let fixture = fixture(true, None, vec![ok()]);
    assert!(!close_tmux_pane_with_dependencies(
        "%42",
        &fixture.dependencies
    ));
    assert!(fixture.calls.lock().unwrap().is_empty());
}

#[test]
fn kill_pane_fails_with_unknown_error_returns_false() {
    let fixture = fixture(
        true,
        Some("tmux"),
        vec![ok(), TmuxCommandResult::new("", "permission denied", 1)],
    );
    assert!(!close_tmux_pane_with_dependencies(
        "%42",
        &fixture.dependencies
    ));
}

#[test]
fn pane_already_closed_by_ctrl_c_returns_true() {
    let fixture = fixture(
        true,
        Some("tmux"),
        vec![ok(), TmuxCommandResult::new("", "can't find pane: %42", 1)],
    );
    assert!(close_tmux_pane_with_dependencies(
        "%42",
        &fixture.dependencies
    ));
}

#[test]
fn cmux_eager_pane_exited_naturally_is_graceful_success() {
    let fixture = fixture(
        true,
        Some("tmux"),
        vec![ok(), TmuxCommandResult::new("", "can't find pane: %55", 1)],
    );
    assert!(close_tmux_pane_with_dependencies(
        "%55",
        &fixture.dependencies
    ));
    assert_eq!(
        fixture.calls.lock().unwrap()[1].1,
        strings(&["kill-pane", "-t", "%55"])
    );
}
