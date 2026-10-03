use std::{sync::{Arc, Mutex}, time::Duration};
use maho_cli::cli::startup_loading_indicator::*;
fn fixture(tty: bool) -> (StartupLoadingIndicator, Arc<Mutex<Vec<String>>>) {
    let writes = Arc::new(Mutex::new(Vec::new())); let output = writes.clone();
    let mut options = StartupLoadingIndicatorOptions::new(move |chunk| output.lock().expect("writes").push(chunk.to_owned()), tty);
    options.frames = vec!["A".to_owned(), "B".to_owned(), "C".to_owned()];
    (StartupLoadingIndicator::new(options), writes)
}
#[tokio::test] async fn first_frame_is_synchronous() { let (mut indicator, writes) = fixture(true); indicator.start(); assert_eq!(writes.lock().unwrap().len(), 1); assert!(writes.lock().unwrap()[0].starts_with("\x1b[?25l\r\x1b[2K")); }
#[tokio::test(start_paused = true)] async fn stop_during_grace_restores_cursor() { let (mut indicator, writes) = fixture(true); indicator.start(); indicator.advance(tokio::time::Instant::now() + Duration::from_millis(119)); indicator.stop(); assert_eq!(writes.lock().unwrap().len(), 2); assert!(writes.lock().unwrap()[1].ends_with("\x1b[?25h")); }
#[tokio::test(start_paused = true)] async fn grace_then_interval_advances_frames() { let (mut indicator, writes) = fixture(true); let now = tokio::time::Instant::now(); indicator.start(); indicator.advance(now + Duration::from_millis(280)); assert_eq!(writes.lock().unwrap().len(), 3); assert!(writes.lock().unwrap()[1].contains('B')); assert!(writes.lock().unwrap()[2].contains('C')); }
#[tokio::test] async fn phase_updates_before_timer() { let (mut indicator, writes) = fixture(true); indicator.start(); indicator.set_phase(Some("phase-value".to_owned())); assert_eq!(writes.lock().unwrap().len(), 2); assert!(writes.lock().unwrap()[1].contains("phase-value")); }
#[tokio::test] async fn pause_suppresses_animation_and_resume_redraws() { let (mut indicator, writes) = fixture(true); indicator.start(); indicator.pause(); indicator.advance(tokio::time::Instant::now() + Duration::from_secs(1)); assert_eq!(writes.lock().unwrap().len(), 2); indicator.resume(); assert_eq!(writes.lock().unwrap().len(), 3); }
#[tokio::test] async fn stop_is_idempotent() { let (mut indicator, writes) = fixture(true); indicator.start(); indicator.stop(); indicator.stop(); assert_eq!(writes.lock().unwrap().len(), 2); assert!(!indicator.running()); }
#[tokio::test] async fn stop_while_paused_emits_nothing() { let (mut indicator, writes) = fixture(true); indicator.start(); indicator.pause(); indicator.stop(); assert_eq!(writes.lock().unwrap().len(), 2); }
#[tokio::test] async fn non_tty_is_inert() { let (mut indicator, writes) = fixture(false); indicator.start(); indicator.stop(); assert!(writes.lock().unwrap().is_empty()); }
#[tokio::test] async fn drop_restores_cursor_and_owns_timer_cleanup() { let (mut indicator, writes) = fixture(true); indicator.start(); drop(indicator); assert_eq!(writes.lock().unwrap().len(), 2); }
#[test] fn only_interactive_tty_without_help_engages() { use maho_core::project_trust::AppMode; assert!(should_show_startup_loading_indicator(AppMode::Interactive, true, false)); for mode in [AppMode::Print, AppMode::Json, AppMode::Rpc, AppMode::AppServer] { assert!(!should_show_startup_loading_indicator(mode, true, false)); } assert!(!should_show_startup_loading_indicator(AppMode::Interactive, false, false)); assert!(!should_show_startup_loading_indicator(AppMode::Interactive, true, true)); }
#[tokio::test] async fn prompt_results_pass_through() { let (mut indicator, _) = fixture(true); indicator.start(); assert_eq!(indicator.during_prompt(async { 42 }).await, 42); assert!(indicator.running()); }
#[tokio::test] async fn rejected_prompt_resumes() { let (mut indicator, writes) = fixture(true); indicator.start(); let result: Result<(), ()> = indicator.during_prompt(async { Err(()) }).await; assert!(result.is_err()); assert_eq!(writes.lock().unwrap().len(), 3); }
