//! Tests matching upstream `test/command.test.ts`.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use senpi_desktop_tool::command::{run_computer_command, COMPUTER_COMMAND_USAGE};
use senpi_desktop_tool::settings::ComputerSettingsInput;

use crate::fixtures;
use fixtures::{desktop_fixture, host_context, methods_of};

#[tokio::test]
async fn prints_live_stop_path_focus_guard_and_permissions_in_status_once_active() {
    let fixture = desktop_fixture(None, HashMap::new());
    let ctx = host_context(None);
    run_computer_command("on", &fixture.handle, &ctx).await;

    let text = run_computer_command("status", &fixture.handle, &ctx).await;
    assert!(text.contains("stopPath=global"));
    assert!(text.contains("focusGuard=true"));
    assert!(text.contains("capturePermission=granted"));
}

#[tokio::test]
async fn reports_status_without_starting_an_engine() {
    let fixture = desktop_fixture(None, HashMap::new());
    let ctx = host_context(None);

    let text = run_computer_command("status", &fixture.handle, &ctx).await;
    assert!(text.contains("stopPath=n/a focusGuard=n/a"));
    assert_eq!(fixture.log.children_count.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn sends_stop_path_resume_exactly_once_for_computer_resume() {
    let fixture = desktop_fixture(None, HashMap::new());
    let ctx = host_context(None);
    run_computer_command("on", &fixture.handle, &ctx).await;
    run_computer_command("stop", &fixture.handle, &ctx).await;

    let text = run_computer_command("resume", &fixture.handle, &ctx).await;
    assert!(text.contains("suspended=false"));

    let methods = methods_of(&fixture.log).await;
    let resumes = methods.iter().filter(|m| m.as_str() == "stopPath.resume").count();
    assert_eq!(resumes, 1);
}

#[tokio::test]
async fn reports_latched_stop_path_as_suspended_in_status() {
    let fixture = desktop_fixture(None, HashMap::new());
    let ctx = host_context(None);
    run_computer_command("on", &fixture.handle, &ctx).await;
    run_computer_command("stop", &fixture.handle, &ctx).await;

    let text = run_computer_command("status", &fixture.handle, &ctx).await;
    assert!(text.contains("stop: stopPath=global suspended=true"));
}

#[tokio::test]
async fn latches_stop_path_through_handle_without_any_tui() {
    let fixture = desktop_fixture(None, HashMap::new());
    let ctx = host_context(None);
    fixture.handle.activate(&ctx).await.expect("activate");

    let status = fixture.handle.stop().await.expect("stop").expect("status");
    assert!(status.suspended);
}

#[tokio::test]
async fn arms_stop_chord_when_computer_on_activates_tool() {
    let settings = ComputerSettingsInput {
        stop_hotkey: Some("ctrl+alt+shift+f12".into()),
        ..Default::default()
    };
    let fixture = desktop_fixture(Some(settings), HashMap::new());
    let ctx = host_context(None);

    let changes = Arc::new(std::sync::Mutex::new(Vec::new()));
    let ch_clone = changes.clone();
    fixture.handle.on_activation_change(Arc::new(move |active| {
        ch_clone.lock().unwrap().push(active);
    })).await;

    run_computer_command("on", &fixture.handle, &ctx).await;

    let ch = changes.lock().unwrap().clone();
    assert_eq!(ch, vec![true]);

    let reqs = fixture.log.requests.lock().await.clone();
    let starts: Vec<_> = reqs.iter().filter(|r| r.method == "stopPath.start").collect();
    assert_eq!(starts.len(), 1);
    let params = starts[0].params.as_ref().unwrap();
    assert_eq!(params.get("chord").and_then(|v| v.as_str()), Some("ctrl+alt+shift+f12"));
}

#[tokio::test]
async fn turns_computer_use_off_for_session_so_next_activation_fails_closed() {
    let fixture = desktop_fixture(None, HashMap::new());
    let ctx = host_context(None);
    run_computer_command("on", &fixture.handle, &ctx).await;
    run_computer_command("off", &fixture.handle, &ctx).await;

    let activation_res = fixture.handle.activate(&ctx).await;
    assert!(activation_res.is_err());
}

#[tokio::test]
async fn answers_unknown_subcommand_with_usage_line() {
    let fixture = desktop_fixture(None, HashMap::new());
    let ctx = host_context(None);

    let text = run_computer_command("reset", &fixture.handle, &ctx).await;
    assert_eq!(text, COMPUTER_COMMAND_USAGE);
}
