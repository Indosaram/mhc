//! Ports of test/harness/adaptive-publisher.test.ts and test/harness/output-capture.test.ts.
//!
//! The TS tests drive the publisher with vitest fake timers; the Rust port injects an equivalent
//! manual clock so time advances deterministically instead of sleeping.

use std::sync::{Arc, Mutex};

use maho_agent::harness::context::background_context;
use maho_agent::harness::types::{ShellOutputLimits, ShellOutputRetention, ShellOutputUpdate, ShellOutputView};
use maho_agent::harness::utils::adaptive_publisher::{
    AdaptivePublisher, AdaptivePublisherOptions, PublisherClock,
};
use maho_agent::harness::utils::output_capture::{
    OutputCapture, OutputCaptureHandlers, apply_shell_output_update, sanitize_shell_output,
};

#[derive(Default)]
struct ManualClock {
    state: Mutex<ManualClockState>,
}

#[derive(Default)]
struct ManualClockState {
    now_ms: i64,
    pending: Option<(i64, Box<dyn FnOnce() + Send>)>,
}

impl ManualClock {
    fn advance(&self, ms: i64) {
        let callback = {
            let mut state = self.state.lock().expect("clock poisoned");
            state.now_ms += ms;
            match state.pending.as_ref() {
                Some((deadline, _)) if *deadline <= state.now_ms => state.pending.take().map(|(_, callback)| callback),
                _ => None,
            }
        };
        if let Some(callback) = callback {
            callback();
        }
    }
}

impl PublisherClock for ManualClock {
    fn now_ms(&self) -> i64 {
        self.state.lock().expect("clock poisoned").now_ms
    }

    fn set_timeout(&self, wait_ms: u64, callback: Box<dyn FnOnce() + Send>) {
        let mut state = self.state.lock().expect("clock poisoned");
        state.pending = Some((state.now_ms + wait_ms as i64, callback));
    }

    fn clear_timeout(&self) {
        self.state.lock().expect("clock poisoned").pending = None;
    }
}

fn create_capture(
    clock: Arc<ManualClock>,
    max_bytes: u64,
    max_lines: u64,
    retain: ShellOutputRetention,
    updates: Arc<Mutex<Vec<ShellOutputUpdate>>>,
    errors: Arc<Mutex<Vec<String>>>,
) -> OutputCapture {
    OutputCapture::with_clock(
        Some(&maho_agent::harness::types::ShellOutputCaptureOptions {
            limits: ShellOutputLimits { max_bytes, max_lines, retain: Some(retain) },
            spill: None,
        }),
        background_context(),
        OutputCaptureHandlers {
            on_update: Some(Arc::new(move |update, _context| {
                updates.lock().expect("updates poisoned").push(update);
                Box::pin(async {})
            })),
            on_error: Arc::new(move |message| errors.lock().expect("errors poisoned").push(message)),
        },
        clock,
    )
    .expect("capture constructs")
}

/// The update record used by the baseline-commit test.
type SnapshotUpdate = (Option<String>, String);

fn fold(updates: &[ShellOutputUpdate]) -> Option<ShellOutputView> {
    let mut output: Option<ShellOutputView> = None;
    for update in updates {
        output = Some(apply_shell_output_update(output.as_ref(), update));
    }
    output
}

// adaptive-publisher.test.ts
#[test]
fn bounds_event_count_and_spaces_large_publications_by_encoded_size() {
    let clock = Arc::new(ManualClock::default());
    let value = Arc::new(Mutex::new("a".to_string()));
    let updates: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let snapshot_value = value.clone();
    let publish_updates = updates.clone();
    let publisher = AdaptivePublisher::new(
        AdaptivePublisherOptions {
            snapshot: Box::new(move || snapshot_value.lock().expect("value poisoned").clone()),
            update: Box::new(|_previous: Option<&String>, current: &String| Some(current.clone())),
            measure: Box::new(|update: &String| update.len() as u64),
            publish: Box::new(move |update| publish_updates.lock().expect("updates poisoned").push(update)),
            on_error: Box::new(|message| panic!("{message}")),
            min_interval_ms: Some(100),
            target_bytes_per_second: Some(100),
        },
        clock.clone(),
    );

    publisher.mark_dirty();
    *value.lock().expect("value poisoned") = "x".repeat(100);
    publisher.mark_dirty();
    clock.advance(100);
    assert_eq!(*updates.lock().expect("updates poisoned"), vec!["a".to_string(), "x".repeat(100)]);

    *value.lock().expect("value poisoned") = "held".to_string();
    publisher.mark_dirty();
    clock.advance(999);
    assert_eq!(updates.lock().expect("updates poisoned").len(), 2);
    clock.advance(1);
    assert_eq!(
        *updates.lock().expect("updates poisoned"),
        vec!["a".to_string(), "x".repeat(100), "held".to_string()]
    );
}

#[test]
fn commits_its_baseline_before_a_consumer_throws() {
    let clock = Arc::new(ManualClock::default());
    let value = Arc::new(Mutex::new("a".to_string()));
    let updates: Arc<Mutex<Vec<SnapshotUpdate>>> = Arc::new(Mutex::new(Vec::new()));
    let throw_after_apply = Arc::new(Mutex::new(false));
    let snapshot_value = value.clone();
    let publish_updates = updates.clone();
    let publish_throw = throw_after_apply.clone();
    let publisher = AdaptivePublisher::new(
        AdaptivePublisherOptions {
            snapshot: Box::new(move || snapshot_value.lock().expect("value poisoned").clone()),
            update: Box::new(|previous: Option<&String>, current: &String| {
                Some((previous.cloned(), current.clone()))
            }),
            measure: Box::new(|_update: &SnapshotUpdate| 1),
            publish: Box::new(move |update| {
                publish_updates.lock().expect("updates poisoned").push(update);
                if *publish_throw.lock().expect("flag poisoned") {
                    panic!("consumer failed after apply");
                }
            }),
            on_error: Box::new(|_message| {}),
            min_interval_ms: Some(100),
            target_bytes_per_second: Some(100),
        },
        clock.clone(),
    );

    publisher.mark_dirty();
    clock.advance(100);
    *value.lock().expect("value poisoned") = "ab".to_string();
    *throw_after_apply.lock().expect("flag poisoned") = true;
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| publisher.mark_dirty())).is_err();
    assert!(panicked, "the consumer panic propagates out of markDirty");
    publisher.flush(true);
    assert_eq!(
        *updates.lock().expect("updates poisoned"),
        vec![(None, "a".to_string()), (Some("a".to_string()), "ab".to_string())]
    );

    *throw_after_apply.lock().expect("flag poisoned") = false;
    clock.advance(100);
    *value.lock().expect("value poisoned") = "abc".to_string();
    publisher.mark_dirty();
    assert_eq!(
        updates.lock().expect("updates poisoned").last().cloned(),
        Some((Some("ab".to_string()), "abc".to_string()))
    );
}

// output-capture.test.ts
#[test]
fn removes_invalid_control_characters_without_changing_text_or_line_boundaries() {
    let input = "a\u{0}b\tc\nd\re\u{7}f\u{fff9}g\u{fffb}h😀";
    assert_eq!(sanitize_shell_output(input), "ab\tc\ndefgh😀");

    let updates = Arc::new(Mutex::new(Vec::new()));
    let capture = create_capture(
        Arc::new(ManualClock::default()),
        50,
        100,
        ShellOutputRetention::Tail,
        updates,
        Arc::new(Mutex::new(Vec::new())),
    );
    capture.push_text(input);
    assert_eq!(capture.snapshot().text, "ab\tc\ndefgh😀");
}

#[test]
fn decodes_utf8_split_across_raw_process_chunks() {
    let capture = create_capture(
        Arc::new(ManualClock::default()),
        50,
        100,
        ShellOutputRetention::Tail,
        Arc::new(Mutex::new(Vec::new())),
        Arc::new(Mutex::new(Vec::new())),
    );
    let bytes = "😀".as_bytes();
    capture.push_bytes(&bytes[..2]);
    assert_eq!(capture.snapshot().text, "");
    capture.push_bytes(&bytes[2..]);
    capture.finish();
    assert_eq!(capture.snapshot().text, "😀");
}

#[test]
fn publishes_the_first_bounded_view_immediately_and_trickling_appends_responsively() {
    let clock = Arc::new(ManualClock::default());
    let updates = Arc::new(Mutex::new(Vec::new()));
    let capture = create_capture(clock.clone(), 50, 100, ShellOutputRetention::Tail, updates.clone(), Arc::new(Mutex::new(Vec::new())));

    capture.push_text("one");
    assert_eq!(updates.lock().expect("updates poisoned").len(), 1);
    assert!(matches!(updates.lock().expect("updates poisoned")[0], ShellOutputUpdate::Replace { .. }));

    clock.advance(150);
    capture.push_text(" two");
    {
        let updates = updates.lock().expect("updates poisoned");
        assert_eq!(updates.len(), 2);
        match &updates[1] {
            ShellOutputUpdate::Append { text, .. } => assert_eq!(text, " two"),
            other => panic!("expected append, got {other:?}"),
        }
    }
    assert_eq!(fold(&updates.lock().expect("updates poisoned")).expect("folded").text, "one two");
}

#[test]
fn collapses_a_burst_into_one_trailing_update() {
    let clock = Arc::new(ManualClock::default());
    let updates = Arc::new(Mutex::new(Vec::new()));
    let capture = create_capture(clock.clone(), 50, 100, ShellOutputRetention::Tail, updates.clone(), Arc::new(Mutex::new(Vec::new())));
    capture.push_text("a");
    capture.push_text("b");
    capture.push_text("c");
    assert_eq!(updates.lock().expect("updates poisoned").len(), 1);

    clock.advance(100);
    {
        let updates = updates.lock().expect("updates poisoned");
        assert_eq!(updates.len(), 2);
        match &updates[1] {
            ShellOutputUpdate::Append { text, .. } => assert_eq!(text, "bc"),
            other => panic!("expected append, got {other:?}"),
        }
    }
    assert_eq!(fold(&updates.lock().expect("updates poisoned")).expect("folded").text, "abc");
}

#[test]
fn publishes_a_small_slide_for_post_cap_trickle() {
    let clock = Arc::new(ManualClock::default());
    let updates = Arc::new(Mutex::new(Vec::new()));
    let capture = create_capture(clock.clone(), 10, 100, ShellOutputRetention::Tail, updates.clone(), Arc::new(Mutex::new(Vec::new())));
    capture.push_text("abcdefghij");
    clock.advance(150);
    capture.push_text("k");

    {
        let updates = updates.lock().expect("updates poisoned");
        match &updates[1] {
            ShellOutputUpdate::Slide { drop, text, .. } => {
                assert_eq!(*drop, 1);
                assert_eq!(text, "k");
            }
            other => panic!("expected slide, got {other:?}"),
        }
    }
    let folded = fold(&updates.lock().expect("updates poisoned")).expect("folded");
    assert_eq!(folded.text, "bcdefghijk");
    assert_eq!(folded.truncation.total_bytes, 11);
}

#[test]
fn keeps_the_exact_byte_count_for_a_single_line_larger_than_its_working_buffer() {
    let capture = create_capture(
        Arc::new(ManualClock::default()),
        10,
        100,
        ShellOutputRetention::Tail,
        Arc::new(Mutex::new(Vec::new())),
        Arc::new(Mutex::new(Vec::new())),
    );
    capture.push_text(&"x".repeat(100));
    let snapshot = capture.snapshot();
    assert_eq!(snapshot.text, "x".repeat(10));
    assert_eq!(snapshot.last_line_bytes, Some(100));
    assert!(snapshot.truncation.last_line_partial);
}

#[test]
fn uses_a_cap_bounded_replacement_after_complete_turnover() {
    let clock = Arc::new(ManualClock::default());
    let updates = Arc::new(Mutex::new(Vec::new()));
    let capture = create_capture(clock.clone(), 10, 100, ShellOutputRetention::Tail, updates.clone(), Arc::new(Mutex::new(Vec::new())));
    capture.push_text("abcdefghij");
    capture.push_text(&"x".repeat(100));
    clock.advance(100);

    assert!(matches!(updates.lock().expect("updates poisoned")[1], ShellOutputUpdate::Replace { .. }));
    let folded = fold(&updates.lock().expect("updates poisoned")).expect("folded");
    assert_eq!(folded.text.chars().count(), 10);
    assert_eq!(folded.truncation.total_bytes, 110);
}

#[test]
fn forces_held_state_and_cancels_its_trailing_timer_on_dispose() {
    let clock = Arc::new(ManualClock::default());
    let updates = Arc::new(Mutex::new(Vec::new()));
    let capture = create_capture(clock.clone(), 50, 100, ShellOutputRetention::Tail, updates.clone(), Arc::new(Mutex::new(Vec::new())));
    capture.push_text("a");
    capture.push_text("b");
    capture.flush();
    assert_eq!(fold(&updates.lock().expect("updates poisoned")).expect("folded").text, "ab");
    capture.dispose();
    clock.advance(1_000);
    assert_eq!(updates.lock().expect("updates poisoned").len(), 2);
}

#[test]
fn preserves_the_original_head_after_its_raw_guard_is_crossed() {
    let capture = create_capture(
        Arc::new(ManualClock::default()),
        100,
        2,
        ShellOutputRetention::Head,
        Arc::new(Mutex::new(Vec::new())),
        Arc::new(Mutex::new(Vec::new())),
    );
    capture.push_text(&format!("first\nsecond\n{}", "tail".repeat(100)));
    assert_eq!(capture.snapshot().text, "first\nsecond");
}

#[test]
fn publishes_spill_metadata_without_resending_text() {
    let clock = Arc::new(ManualClock::default());
    let updates = Arc::new(Mutex::new(Vec::new()));
    let errors = Arc::new(Mutex::new(Vec::new()));
    let capture = create_capture(clock.clone(), 50, 100, ShellOutputRetention::Tail, updates.clone(), errors.clone());
    capture.push_text("output");
    capture.set_spill_path("/tmp/output.log");
    {
        let updates = updates.lock().expect("updates poisoned");
        match updates.last().expect("an update") {
            ShellOutputUpdate::Metadata { metadata } => {
                assert_eq!(metadata.spill_path.as_deref(), Some("/tmp/output.log"));
            }
            other => panic!("expected metadata, got {other:?}"),
        }
    }
    assert_eq!(
        fold(&updates.lock().expect("updates poisoned")).expect("folded").spill_path.as_deref(),
        Some("/tmp/output.log")
    );
    assert!(errors.lock().expect("errors poisoned").is_empty());
}
