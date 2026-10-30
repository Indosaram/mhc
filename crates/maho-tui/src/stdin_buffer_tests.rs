//! Port of senpi packages/tui/test/stdin-buffer.test.ts (one #[test] per `it`). Timers run on
//! a manual clock: `wait(ms)` becomes `advance(ms)`, which fires every deadline it passes.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;
use crate::keys::matches_key;

#[derive(Default)]
struct ManualClock(AtomicU64);

impl Clock for ManualClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

struct H {
    clock: Arc<ManualClock>,
    buffer: StdinBuffer,
    data: Vec<String>,
    paste: Vec<String>,
}

impl H {
    fn new(options: StdinBufferOptions) -> Self {
        let clock = Arc::new(ManualClock::default());
        let buffer = StdinBuffer::with_clock(options, Arc::clone(&clock) as Arc<dyn Clock>);
        Self {
            clock,
            buffer,
            data: Vec::new(),
            paste: Vec::new(),
        }
    }

    /// The suite's `beforeEach`: `new StdinBuffer({ timeout: 10 })` with data/paste listeners.
    fn default_test() -> Self {
        Self::new(StdinBufferOptions {
            timeout: Some(10),
            ..Default::default()
        })
    }

    fn collect(&mut self, events: Vec<StdinEvent>) {
        for event in events {
            match event {
                StdinEvent::Data(sequence) => self.data.push(sequence),
                StdinEvent::Paste(content) => self.paste.push(content),
            }
        }
    }

    fn input<'a>(&mut self, data: impl Into<StdinInput<'a>>) {
        let events = self.buffer.process(data);
        self.collect(events);
    }

    /// Advances the clock by `ms`, firing each timer whose deadline falls inside the window.
    fn advance(&mut self, ms: u64) {
        let target = self.clock.now_ms() + ms;
        while let Some(deadline) = self.buffer.deadline().filter(|d| *d <= target) {
            self.clock.0.store(deadline, Ordering::SeqCst);
            let events = self.buffer.fire_timer();
            self.collect(events);
        }
        self.clock.0.store(target, Ordering::SeqCst);
    }

    /// `once(buffer, "data", { signal: AbortSignal.timeout(limit) })`: advance until the first
    /// data event, failing if none arrives within `limit` ms.
    fn advance_until_data(&mut self, limit: u64) {
        let start = self.clock.now_ms();
        while self.data.is_empty() {
            let deadline = self.buffer.deadline().expect("a pending timer");
            assert!(deadline - start <= limit, "no data event within {limit}ms");
            self.advance(deadline - self.clock.now_ms());
        }
    }
}

#[test]
fn should_pass_through_regular_characters_immediately() {
    let mut h = H::default_test();
    h.input("a");
    assert_eq!(h.data, ["a"]);
}

#[test]
fn should_pass_through_multiple_regular_characters() {
    let mut h = H::default_test();
    h.input("abc");
    assert_eq!(h.data, ["a", "b", "c"]);
}

#[test]
fn should_handle_unicode_characters() {
    let mut h = H::default_test();
    h.input("hello 世界");
    assert_eq!(h.data, ["h", "e", "l", "l", "o", " ", "世", "界"]);
}

#[test]
fn should_pass_through_complete_mouse_sgr_sequences() {
    let mut h = H::default_test();
    let mouse_seq = "\x1b[<35;20;5m";
    h.input(mouse_seq);
    assert_eq!(h.data, [mouse_seq]);
}

#[test]
fn should_pass_through_complete_arrow_key_sequences() {
    let mut h = H::default_test();
    let up_arrow = "\x1b[A";
    h.input(up_arrow);
    assert_eq!(h.data, [up_arrow]);
}

#[test]
fn should_pass_through_complete_function_key_sequences() {
    let mut h = H::default_test();
    let f1 = "\x1b[11~";
    h.input(f1);
    assert_eq!(h.data, [f1]);
}

#[test]
fn should_pass_through_meta_key_sequences() {
    let mut h = H::default_test();
    let meta_a = "\x1ba";
    h.input(meta_a);
    assert_eq!(h.data, [meta_a]);
}

#[test]
fn should_pass_through_ss3_sequences() {
    let mut h = H::default_test();
    let ss3 = "\x1bOA";
    h.input(ss3);
    assert_eq!(h.data, [ss3]);
}

#[test]
fn should_buffer_incomplete_mouse_sgr_sequence() {
    let mut h = H::default_test();
    h.input("\x1b");
    assert!(h.data.is_empty());
    assert_eq!(h.buffer.get_buffer(), "\x1b");
    h.input("[<35");
    assert!(h.data.is_empty());
    assert_eq!(h.buffer.get_buffer(), "\x1b[<35");
    h.input(";20;5m");
    assert_eq!(h.data, ["\x1b[<35;20;5m"]);
    assert_eq!(h.buffer.get_buffer(), "");
}

#[test]
fn should_buffer_incomplete_csi_sequence() {
    let mut h = H::default_test();
    h.input("\x1b[");
    assert!(h.data.is_empty());
    h.input("1;");
    assert!(h.data.is_empty());
    h.input("5H");
    assert_eq!(h.data, ["\x1b[1;5H"]);
}

#[test]
fn should_buffer_split_across_many_chunks() {
    let mut h = H::default_test();
    h.input("\x1b");
    h.input("[");
    h.input("<");
    h.input("3");
    h.input("5");
    h.input(";");
    h.input("2");
    h.input("0");
    h.input(";");
    h.input("5");
    h.input("m");
    assert_eq!(h.data, ["\x1b[<35;20;5m"]);
}

#[test]
fn should_flush_an_unowned_incomplete_csi_after_timeout() {
    let mut h = H::default_test();
    h.input("\x1b[35");
    assert!(h.data.is_empty());
    // once(buffer, "data", { signal: AbortSignal.timeout(1000) })
    h.advance_until_data(1000);
    assert_eq!(h.data, ["\x1b[35"]);
}

#[test]
fn should_flush_a_lone_esc_as_escape_when_cr_arrives_after_the_timeout() {
    let mut h = H::new(StdinBufferOptions {
        timeout: Some(10),
        escape_timeout: Some(10),
        ..Default::default()
    });
    // Legacy-mode Alt+Enter is ESC + CR; when the terminal/transport splits
    // the bytes further apart than the timeout, ESC is flushed alone and the
    // host sees Escape (interrupt) instead of Alt+Enter. This locks in the
    // behavior so the configurable timeout in ProcessTerminal stays honest.
    h.input("\x1b");
    h.advance(20);
    h.input("\r");
    assert_eq!(h.data, ["\x1b", "\r"]);
    assert!(matches_key(&h.data[0], "escape"));
}

#[test]
fn should_merge_esc_cr_split_across_chunks_within_a_larger_timeout() {
    let mut h = H::new(StdinBufferOptions {
        escape_timeout: Some(100),
        ..Default::default()
    });
    h.input("\x1b");
    h.advance(20); // > 10ms default escapeTimeout, < 100ms configured escapeTimeout
    h.input("\r");
    assert_eq!(h.data, ["\x1b\r"]);
    assert!(matches_key(&h.data[0], "alt+enter"));
}

#[test]
fn does_not_apply_the_sequence_timeout_to_a_lone_esc() {
    let mut h = H::new(StdinBufferOptions {
        timeout: Some(100),
        escape_timeout: Some(10),
        ..Default::default()
    });
    h.input("\x1b");
    h.advance(20);
    h.input("\r");
    assert_eq!(h.data, ["\x1b", "\r"]);
    assert!(matches_key(&h.data[0], "escape"));
}

#[test]
fn keeps_fragmented_mouse_sequences_buffered_across_delayed_chunks_by_default() {
    let mut h = H::new(StdinBufferOptions::default());
    h.input("\x1b[");
    h.advance(20);
    assert!(h.data.is_empty());
    h.input("<65;48;39M");
    assert_eq!(h.data, ["\x1b[<65;48;39M"]);
    h.buffer.destroy();
}

#[test]
fn should_handle_characters_followed_by_escape_sequence() {
    let mut h = H::default_test();
    h.input("abc\x1b[A");
    assert_eq!(h.data, ["a", "b", "c", "\x1b[A"]);
}

#[test]
fn should_handle_escape_sequence_followed_by_characters() {
    let mut h = H::default_test();
    h.input("\x1b[Aabc");
    assert_eq!(h.data, ["\x1b[A", "a", "b", "c"]);
}

#[test]
fn should_handle_multiple_complete_sequences() {
    let mut h = H::default_test();
    h.input("\x1b[A\x1b[B\x1b[C");
    assert_eq!(h.data, ["\x1b[A", "\x1b[B", "\x1b[C"]);
}

#[test]
fn should_handle_partial_sequence_with_preceding_characters() {
    let mut h = H::default_test();
    h.input("abc\x1b[<35");
    assert_eq!(h.data, ["a", "b", "c"]);
    assert_eq!(h.buffer.get_buffer(), "\x1b[<35");
    h.input(";20;5m");
    assert_eq!(h.data, ["a", "b", "c", "\x1b[<35;20;5m"]);
}

#[test]
fn should_handle_kitty_csi_u_press_events() {
    let mut h = H::default_test();
    // Press 'a' in Kitty protocol
    h.input("\x1b[97u");
    assert_eq!(h.data, ["\x1b[97u"]);
}

#[test]
fn should_handle_kitty_csi_u_release_events() {
    let mut h = H::default_test();
    // Release 'a' in Kitty protocol
    h.input("\x1b[97;1:3u");
    assert_eq!(h.data, ["\x1b[97;1:3u"]);
}

#[test]
fn should_handle_batched_kitty_press_and_release() {
    let mut h = H::default_test();
    // Press 'a', release 'a' batched together (common over SSH)
    h.input("\x1b[97u\x1b[97;1:3u");
    assert_eq!(h.data, ["\x1b[97u", "\x1b[97;1:3u"]);
}

#[test]
fn should_handle_multiple_batched_kitty_events() {
    let mut h = H::default_test();
    // Press 'a', release 'a', press 'b', release 'b'
    h.input("\x1b[97u\x1b[97;1:3u\x1b[98u\x1b[98;1:3u");
    assert_eq!(
        h.data,
        ["\x1b[97u", "\x1b[97;1:3u", "\x1b[98u", "\x1b[98;1:3u"]
    );
}

#[test]
fn should_handle_kitty_arrow_keys_with_event_type() {
    let mut h = H::default_test();
    // Up arrow press with event type
    h.input("\x1b[1;1:1A");
    assert_eq!(h.data, ["\x1b[1;1:1A"]);
}

#[test]
fn should_handle_kitty_functional_keys_with_event_type() {
    let mut h = H::default_test();
    // Delete key release
    h.input("\x1b[3;1:3~");
    assert_eq!(h.data, ["\x1b[3;1:3~"]);
}

#[test]
fn should_keep_split_tmux_csi_u_shift_enter_as_one_sequence() {
    let mut h = H::default_test();
    let given_first_chunk = "\x1b[13";
    let given_second_chunk = ";2u";
    let then_complete_sequence = "\x1b[13;2u";
    h.input(given_first_chunk);
    assert!(h.data.is_empty());
    assert_eq!(h.buffer.get_buffer(), given_first_chunk);
    h.input(given_second_chunk);
    assert_eq!(h.data, [then_complete_sequence]);
    assert_eq!(h.buffer.get_buffer(), "");
}

#[test]
fn should_split_esc_esc_csi_into_standalone_esc_and_the_csi_sequence_wezterm_escape_key_regression()
{
    let mut h = H::default_test();
    // WezTerm with enable_kitty_keyboard sends Escape key press as raw \x1b
    // and the release as a full Kitty CSI-u sequence, concatenated.
    // The buffer must not treat \x1b\x1b as a complete meta-key when the
    // following byte starts a new escape sequence.
    h.input("\x1b\x1b[27;129:3u");
    assert_eq!(h.data, ["\x1b", "\x1b[27;129:3u"]);
}

#[test]
fn should_split_esc_esc_csi_with_no_modifier_no_num_lock() {
    let mut h = H::default_test();
    h.input("\x1b\x1b[27;1:3u");
    assert_eq!(h.data, ["\x1b", "\x1b[27;1:3u"]);
}

#[test]
fn should_still_emit_esc_esc_as_a_single_sequence_when_not_followed_by_a_new_escape() {
    let mut h = H::default_test();
    // \x1b\x1b alone (no following CSI) stays as-is — e.g. ctrl+alt+[
    h.input("\x1b\x1b");
    assert_eq!(h.data, ["\x1b\x1b"]);
}

#[test]
fn should_handle_plain_characters_mixed_with_kitty_sequences() {
    let mut h = H::default_test();
    // Plain 'a' followed by Kitty release
    h.input("a\x1b[97;1:3u");
    assert_eq!(h.data, ["a", "\x1b[97;1:3u"]);
}

#[test]
fn should_drop_raw_duplicate_character_after_matching_kitty_printable_sequence() {
    let mut h = H::default_test();
    h.input("\x1b[224uà");
    assert_eq!(h.data, ["\x1b[224u"]);
}

#[test]
fn should_drop_raw_duplicate_character_after_matching_kitty_printable_sequence_across_chunks() {
    let mut h = H::default_test();
    h.input("\x1b[64u");
    h.input("@");
    assert_eq!(h.data, ["\x1b[64u"]);
}

#[test]
fn should_keep_non_matching_plain_character_after_kitty_printable_sequence() {
    let mut h = H::default_test();
    h.input("\x1b[97ub");
    assert_eq!(h.data, ["\x1b[97u", "b"]);
}

#[test]
fn should_keep_raw_character_after_modified_kitty_printable_sequence() {
    let mut h = H::default_test();
    h.input("\x1b[64;3u@");
    assert_eq!(h.data, ["\x1b[64;3u", "@"]);
}

#[test]
fn should_handle_rapid_typing_simulation_with_kitty_protocol() {
    let mut h = H::default_test();
    // Simulates typing "hi" quickly with releases interleaved
    h.input("\x1b[104u\x1b[104;1:3u\x1b[105u\x1b[105;1:3u");
    assert_eq!(
        h.data,
        ["\x1b[104u", "\x1b[104;1:3u", "\x1b[105u", "\x1b[105;1:3u"]
    );
}

#[test]
fn should_handle_mouse_press_event() {
    let mut h = H::default_test();
    h.input("\x1b[<0;10;5M");
    assert_eq!(h.data, ["\x1b[<0;10;5M"]);
}

#[test]
fn should_handle_mouse_release_event() {
    let mut h = H::default_test();
    h.input("\x1b[<0;10;5m");
    assert_eq!(h.data, ["\x1b[<0;10;5m"]);
}

#[test]
fn should_handle_mouse_move_event() {
    let mut h = H::default_test();
    h.input("\x1b[<35;20;5m");
    assert_eq!(h.data, ["\x1b[<35;20;5m"]);
}

#[test]
fn should_handle_split_mouse_events() {
    let mut h = H::default_test();
    h.input("\x1b[<3");
    h.input("5;1");
    h.input("5;");
    h.input("10m");
    assert_eq!(h.data, ["\x1b[<35;15;10m"]);
}

#[test]
fn should_handle_multiple_mouse_events() {
    let mut h = H::default_test();
    h.input("\x1b[<35;1;1m\x1b[<35;2;2m\x1b[<35;3;3m");
    assert_eq!(h.data, ["\x1b[<35;1;1m", "\x1b[<35;2;2m", "\x1b[<35;3;3m"]);
}

#[test]
fn should_handle_old_style_mouse_sequence_esc_m_3_bytes() {
    let mut h = H::default_test();
    h.input("\x1b[M abc");
    assert_eq!(h.data, ["\x1b[M ab", "c"]);
}

#[test]
fn should_buffer_incomplete_old_style_mouse_sequence() {
    let mut h = H::default_test();
    h.input("\x1b[M");
    assert_eq!(h.buffer.get_buffer(), "\x1b[M");
    h.input(" a");
    assert_eq!(h.buffer.get_buffer(), "\x1b[M a");
    h.input("b");
    assert_eq!(h.data, ["\x1b[M ab"]);
}

#[test]
fn should_handle_empty_input() {
    let mut h = H::default_test();
    h.input("");
    // Empty string emits an empty data event
    assert_eq!(h.data, [""]);
}

#[test]
fn should_handle_lone_escape_character_with_timeout() {
    let mut h = H::new(StdinBufferOptions {
        timeout: Some(10),
        escape_timeout: Some(10),
        ..Default::default()
    });
    h.input("\x1b");
    assert!(h.data.is_empty());
    // After timeout, should emit
    h.advance(15);
    assert_eq!(h.data, ["\x1b"]);
}

#[test]
fn flushes_a_lone_escape_promptly_with_a_configured_short_escape_timeout() {
    let mut h = H::new(StdinBufferOptions {
        escape_timeout: Some(10),
        ..Default::default()
    });
    h.input("\x1b");
    h.advance(20);
    assert_eq!(h.data, ["\x1b"]);
    h.buffer.destroy();
}

#[test]
fn should_handle_lone_escape_character_with_explicit_flush() {
    let mut h = H::default_test();
    h.input("\x1b");
    assert!(h.data.is_empty());
    let flushed = h.buffer.flush();
    assert_eq!(flushed, ["\x1b"]);
}

#[test]
fn should_handle_buffer_input() {
    let mut h = H::default_test();
    h.input("\x1b[A".as_bytes());
    assert_eq!(h.data, ["\x1b[A"]);
}

#[test]
fn should_not_emit_an_empty_event_when_a_buffer_chunk_only_contains_a_partial_utf_8_prefix() {
    let mut h = H::default_test();
    h.input(&[0xe4_u8, 0xb8][..]);
    assert!(h.data.is_empty());
}

#[test]
fn should_handle_very_long_sequences() {
    let mut h = H::default_test();
    let long_seq = format!("\x1b[{}H", "1;".repeat(50));
    h.input(long_seq.as_str());
    assert_eq!(h.data, [long_seq]);
}

#[test]
fn should_retain_owned_mouse_fragments_on_explicit_flush_1645() {
    let mut h = H::default_test();
    h.input("\x1b[<35");
    assert!(h.buffer.flush().is_empty());
    assert_eq!(h.buffer.get_buffer(), "\x1b[<35");
    h.input(";20;5M");
    assert_eq!(h.data, ["\x1b[<35;20;5M"]);
}

#[test]
fn should_return_empty_array_if_nothing_to_flush() {
    let mut h = H::default_test();
    let flushed = h.buffer.flush();
    assert!(flushed.is_empty());
}

#[test]
fn should_emit_unowned_flushed_data_via_timeout() {
    let mut h = H::default_test();
    h.input("\x1b[35");
    assert!(h.data.is_empty());
    // once(buffer, "data", { signal: AbortSignal.timeout(1000) })
    h.advance_until_data(1000);
    assert_eq!(h.data, ["\x1b[35"]);
}

#[test]
fn should_clear_buffered_content_without_emitting() {
    let mut h = H::default_test();
    h.input("\x1b[<35");
    assert_eq!(h.buffer.get_buffer(), "\x1b[<35");
    h.buffer.clear();
    assert_eq!(h.buffer.get_buffer(), "");
    assert!(h.data.is_empty());
}

#[test]
fn should_reset_incomplete_utf_8_bytes_on_clear() {
    let mut h = H::default_test();
    h.input(&[0xe4_u8, 0xb8][..]);
    h.buffer.clear();
    h.input(&[0xad_u8][..]);
    assert_ne!(h.data.concat(), "中");
}

#[test]
fn should_emit_paste_event_for_complete_bracketed_paste() {
    let mut h = H::default_test();
    let paste_start = "\x1b[200~";
    let paste_end = "\x1b[201~";
    let content = "hello world";
    h.input(format!("{paste_start}{content}{paste_end}").as_str());
    assert_eq!(h.paste, ["hello world"]);
    assert!(h.data.is_empty()); // No data events during paste
}

#[test]
fn should_group_unbracketed_multiline_burst_as_paste() {
    let mut h = H::new(StdinBufferOptions::default());
    h.input("first\nsecond\n");
    assert_eq!(h.paste, ["first\nsecond\n"]);
}

#[test]
fn should_group_fast_unbracketed_multiline_input_as_paste() {
    let mut h = H::new(StdinBufferOptions {
        burst_window_ms: Some(20),
        ..Default::default()
    });
    h.input("first\nsecond\n");
    assert_eq!(h.paste, ["first\nsecond\n"]);
    assert!(h.data.is_empty());
}

#[test]
fn should_hold_trailing_newline_after_fast_text_for_fragment_continuation() {
    let mut h = H::new(StdinBufferOptions {
        burst_window_ms: Some(20),
        ..Default::default()
    });
    h.input("a");
    h.advance(5);
    h.input("b\n");
    assert_eq!(h.data, ["a", "b"]);
    h.advance(20);
    assert_eq!(h.data, ["a", "b", "\n"]);
}

#[test]
fn should_handle_paste_arriving_in_chunks() {
    let mut h = H::default_test();
    h.input("\x1b[200~");
    assert!(h.paste.is_empty());
    h.input("hello ");
    assert!(h.paste.is_empty());
    h.input("world\x1b[201~");
    assert_eq!(h.paste, ["hello world"]);
    assert!(h.data.is_empty());
}

#[test]
fn should_handle_paste_with_input_before_and_after() {
    let mut h = H::default_test();
    h.input("a");
    h.input("\x1b[200~pasted\x1b[201~");
    h.input("b");
    assert_eq!(h.data, ["a", "b"]);
    assert_eq!(h.paste, ["pasted"]);
}

#[test]
fn should_handle_paste_with_newlines() {
    let mut h = H::default_test();
    h.input("\x1b[200~line1\nline2\nline3\x1b[201~");
    assert_eq!(h.paste, ["line1\nline2\nline3"]);
    assert!(h.data.is_empty());
}

#[test]
fn should_handle_paste_with_unicode() {
    let mut h = H::default_test();
    h.input("\x1b[200~Hello 世界 🎉\x1b[201~");
    assert_eq!(h.paste, ["Hello 世界 🎉"]);
    assert!(h.data.is_empty());
}

#[test]
fn should_reassemble_cjk_paste_content_and_an_end_marker_split_across_buffer_chunks() {
    let mut h = H::default_test();
    let payload = "\x1b[200~中文\x1b[201~".as_bytes();
    h.input(&payload[0..9]);
    h.input(&payload[9..payload.len() - 3]);
    h.input(&payload[payload.len() - 3..]);
    assert_eq!(h.paste, ["中文"]);
    assert!(!h.paste.concat().contains('\u{FFFD}'));
}

#[test]
fn should_reassemble_a_cjk_code_point_split_across_paste_content_buffer_chunks() {
    let mut h = H::default_test();
    h.input("\x1b[200~");
    h.input(&[0xe4_u8, 0xb8][..]);
    h.input(&[&[0xad_u8][..], "\x1b[201~".as_bytes()].concat()[..]);
    assert_eq!(h.paste, ["中"]);
    assert!(!h.paste.concat().contains('\u{FFFD}'));
}

#[test]
fn should_terminate_paste_mode_when_the_end_marker_is_split_across_chunks() {
    let mut h = H::default_test();
    h.input("\x1b[200~hello\x1b[20");
    h.input("1~a");
    assert_eq!(h.paste, ["hello"]);
    assert_eq!(h.data, ["a"]);
}

#[test]
fn should_clear_buffer_on_destroy() {
    let mut h = H::default_test();
    h.input("\x1b[<35");
    assert_eq!(h.buffer.get_buffer(), "\x1b[<35");
    h.buffer.destroy();
    assert_eq!(h.buffer.get_buffer(), "");
}

#[test]
fn should_clear_pending_timeouts_on_destroy() {
    let mut h = H::default_test();
    h.input("\x1b[<35");
    h.buffer.destroy();
    // Wait longer than timeout
    h.advance(15);
    // Should not have emitted anything
    assert!(h.data.is_empty());
}

#[test]
fn should_reset_incomplete_utf_8_bytes_on_destroy() {
    let mut h = H::default_test();
    h.input(&[0xe4_u8, 0xb8][..]);
    h.buffer.destroy();
    h.input(&[0xad_u8][..]);
    assert_ne!(h.data.concat(), "中");
}

#[test]
fn should_reassemble_a_3_byte_cjk_code_point_split_across_two_buffer_chunks() {
    let mut h = H::default_test();
    h.input(&[0xe4_u8, 0xb8][..]);
    assert!(h.data.is_empty());
    h.input(&[0xad_u8][..]);
    assert_eq!(h.data, ["中"]);
    assert!(!h.data.concat().contains('\u{FFFD}'));
}

#[test]
fn should_reassemble_4_byte_emoji_split_1_3_and_3_1() {
    let mut h = H::default_test();
    let emoji = "🎉".as_bytes();
    h.input(&emoji[0..1]);
    h.input(&emoji[1..]);
    assert_eq!(h.data, ["🎉"]);
    h.data.clear();
    h.input(&emoji[0..3]);
    h.input(&emoji[3..]);
    assert_eq!(h.data, ["🎉"]);
}

#[test]
fn should_hold_trailing_incomplete_utf_8_bytes_across_flush_until_completion() {
    let mut h = H::default_test();
    h.input(&[0xe4_u8, 0xb8][..]);
    assert!(h.data.is_empty());
    assert!(h.buffer.flush().is_empty());
    h.input(&[0xad_u8][..]);
    assert_eq!(h.data, ["中"]);
    assert!(!h.data.concat().contains('\u{FFFD}'));
}

#[test]
fn should_preserve_legacy_single_byte_high_bit_meta_conversion() {
    let mut h = H::default_test();
    h.input(&[0x9b_u8][..]);
    assert_eq!(h.data, ["\x1b\u{1b}"]);
}

// ---- Escape window decided by arrival gaps (#3628) ----
// Alt+<key> arrives as ESC + key. Terminals on WSL and multiplexer relays can deliver the two
// bytes as separate reads; whether they merge into one alt-key sequence must be decided by the
// observed arrival gap, not by whether the flush timer callback happened to run first.

/// The suite's gap `beforeEach`: `new StdinBuffer({ escapeTimeout: 50, now: () => clockNow })`.
fn gap_buffer() -> H {
    let h = H::new(StdinBufferOptions {
        escape_timeout: Some(50),
        ..Default::default()
    });
    h.clock.0.store(1_000, Ordering::SeqCst);
    h
}

/// Moves the manual clock without firing timers (the upstream `clockNow += ms`).
fn bump(h: &H, ms: u64) {
    h.clock.0.fetch_add(ms, Ordering::SeqCst);
}

#[test]
fn merges_esc_and_a_printable_key_arriving_within_the_escape_window() {
    let mut h = gap_buffer();
    h.input("\x1b");
    bump(&h, 30);
    h.input("q");
    assert_eq!(h.data, ["\x1bq"]);
    assert!(matches_key(&h.data[0], "alt+q"));
}

#[test]
fn releases_a_stale_lone_esc_when_the_next_key_arrives_after_the_escape_window() {
    let mut h = gap_buffer();
    h.input("\x1b");
    bump(&h, 100);
    h.input("q");
    assert_eq!(h.data, ["\x1b", "q"]);
    assert!(matches_key(&h.data[0], "escape"));
}

#[test]
fn keeps_the_default_escape_window_at_the_standard_50ms_for_split_alt_delivery() {
    let mut h = H::new(StdinBufferOptions::default());
    h.clock.0.store(1_000, Ordering::SeqCst);
    h.input("\x1b");
    bump(&h, 30);
    h.input("q");
    assert_eq!(h.data, ["\x1bq"]);
    assert!(matches_key(&h.data[0], "alt+q"));
    h.buffer.destroy();
}

// ---- StdinBuffer unbracketed paste bursts ----

/// `new StdinBuffer({ timeout: 10, burstWindowMs: 20, now: () => now })` with `now = 1000`.
fn burst_buffer() -> H {
    let h = H::new(StdinBufferOptions {
        timeout: Some(10),
        burst_window_ms: Some(20),
        ..Default::default()
    });
    h.clock.0.store(1_000, Ordering::SeqCst);
    h
}

/// `burst.flush()`: releases a held line break; returned sequences are not emitted.
fn burst_flush(h: &mut H) {
    let (events, _sequences) = h.buffer.flush_with_events();
    h.collect(events);
}

#[test]
fn emits_a_one_chunk_multiline_burst_as_a_single_paste_and_submits_nothing() {
    let mut h = burst_buffer();
    h.input("line1\nline2\nline3");
    assert_eq!(h.paste, ["line1\nline2\nline3"]);
    assert!(h.data.is_empty());
}

#[test]
fn coalesces_a_burst_split_across_chunks_within_the_window_into_one_block() {
    let mut h = burst_buffer();
    h.input("line1\nline2\n");
    h.input("line3\n");
    assert_eq!(h.paste, ["line1\nline2\n"]);
    assert_eq!(h.data, ["l", "i", "n", "e", "3"]);
    burst_flush(&mut h);
    assert_eq!(h.paste, ["line1\nline2\n", "\n"]);
    assert_eq!(h.data, ["l", "i", "n", "e", "3"]);
}

#[test]
fn submits_a_keystroke_paced_enter_immediately_without_holding() {
    let mut h = burst_buffer();
    h.input("a");
    bump(&h, 1000);
    h.input("\n");
    assert_eq!(h.data, ["a", "\n"]);
    assert!(h.paste.is_empty());
}

#[test]
fn holds_a_burst_paced_trailing_newline_until_flush_then_submits_once() {
    let mut h = burst_buffer();
    h.input("a");
    bump(&h, 5);
    h.input("b\n");
    assert_eq!(h.data, ["a", "b"]);
    burst_flush(&mut h);
    assert_eq!(h.data, ["a", "b", "\n"]);
    assert!(h.paste.is_empty());
}

#[test]
fn forwards_a_bare_enter_read_at_once_even_right_after_other_input() {
    let mut h = burst_buffer();
    h.input("\n");
    h.input("\r");
    assert_eq!(h.data, ["\n", "\r"]);
    assert!(h.paste.is_empty());
}

#[test]
fn emits_a_first_ever_single_line_input_immediately() {
    let mut h = burst_buffer();
    h.input("solo\n");
    assert_eq!(h.data, ["s", "o", "l", "o", "\n"]);
    assert!(h.paste.is_empty());
}

#[test]
fn keeps_a_fast_double_enter_as_two_submits_instead_of_a_paste() {
    let mut h = burst_buffer();
    h.input("a");
    h.input("\n");
    h.input("\n");
    assert!(h.paste.is_empty());
    burst_flush(&mut h);
    assert_eq!(h.data, ["a", "\n", "\n"]);
}

#[test]
fn leaves_bracketed_pastes_on_the_existing_marker_path() {
    let mut h = burst_buffer();
    h.input("\x1b[200~a\nb\x1b[201~");
    assert_eq!(h.paste, ["a\nb"]);
    assert!(h.data.is_empty());
}

#[test]
fn treats_a_one_chunk_two_line_paste_as_a_paste_instead_of_submitting_its_first_line() {
    let mut h = burst_buffer();
    h.input("first\nsecond");
    assert_eq!(h.paste, ["first\nsecond"]);
    assert!(h.data.is_empty());
}

#[test]
fn keeps_the_newline_that_ends_a_paste_split_across_reads_inside_the_paste() {
    let mut h = burst_buffer();
    h.input("line1\nline2\n");
    bump(&h, 5);
    h.input("line3\n");
    burst_flush(&mut h);
    assert_eq!(h.data, ["l", "i", "n", "e", "3"]);
    assert_eq!(h.paste, ["line1\nline2\n", "\n"]);
}

#[test]
fn submits_exactly_once_when_a_lone_enter_follows_a_paste_after_a_pause() {
    let mut h = burst_buffer();
    h.input("line1\nline2\nline3");
    bump(&h, 1000);
    h.input("\r");
    assert_eq!(h.paste, ["line1\nline2\nline3"]);
    assert_eq!(h.data, ["\r"]);
}

#[test]
fn releases_a_held_newline_on_time_even_when_an_empty_read_arrives_while_it_is_held() {
    let mut h = H::new(StdinBufferOptions {
        timeout: Some(10),
        burst_window_ms: Some(20),
        ..Default::default()
    });
    h.input("a");
    h.input("b\n");
    h.input(&[0xf0_u8, 0x9f][..]);
    // Bounded wait for the release timer (upstream: a 2000ms bound on the "\n" data event).
    let start = h.clock.now_ms();
    while !h.data.iter().any(|sequence| sequence == "\n") {
        let deadline = h
            .buffer
            .deadline()
            .expect("held newline was never released");
        assert!(deadline - start <= 2000, "held newline was never released");
        h.advance(deadline - h.clock.now_ms());
    }
    assert_eq!(h.data, ["a", "b", "\n"]);
    assert!(h.paste.is_empty());
    h.buffer.destroy();
}

#[test]
fn does_not_glue_a_newline_held_before_an_empty_read_into_a_later_read_s_paste() {
    let mut h = burst_buffer();
    h.input("a");
    bump(&h, 5);
    h.input("b\n");
    h.input(&[0xf0_u8, 0x9f][..]);
    bump(&h, 1000);
    h.input(&[0x98_u8, 0x80, 0x0a, 0x78][..]);
    assert_eq!(h.data[..3], ["a", "b", "\n"]);
    assert!(
        h.paste.iter().all(|paste| !paste.starts_with('\n')),
        "{:?}",
        h.paste
    );
}

#[test]
fn treats_crlf_and_cr_only_bursts_like_lf_bursts() {
    let mut h = burst_buffer();
    h.input("l1\r\nl2\r\n");
    assert_eq!(h.paste, ["l1\r\nl2\r\n"]);
    h.input("m1\rm2\r");
    assert_eq!(h.paste, ["l1\r\nl2\r\n", "m1\rm2\r"]);
}
