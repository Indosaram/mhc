//! Port of senpi packages/tui/test/word-navigation.test.ts. Cursors are UTF-8 byte offsets in
//! Rust; the CJK cases convert senpi's UTF-16 indices (each CJK char is 1 unit, 3 bytes).

use super::*;

fn back(text: &str, cursor: usize) -> usize {
    find_word_backward(text, cursor, &WordNavigationOptions::default())
}

fn fwd(text: &str, cursor: usize) -> usize {
    find_word_forward(text, cursor, &WordNavigationOptions::default())
}

mod find_word_backward_tests {
    use super::*;

    #[test]
    fn basic_words_hello_world() {
        assert_eq!(back("hello world", 11), 6);
        assert_eq!(back("hello world", 6), 0);
    }

    #[test]
    fn dotted_foo_bar() {
        assert_eq!(back("foo.bar", 7), 4);
        assert_eq!(back("foo.bar", 4), 3);
        assert_eq!(back("foo.bar", 3), 0);
    }

    #[test]
    fn colon_foo_bar() {
        assert_eq!(back("foo:bar", 7), 4);
        assert_eq!(back("foo:bar", 4), 3);
        assert_eq!(back("foo:bar", 3), 0);
    }

    #[test]
    fn path_path_to_file() {
        let text = "path/to/file";
        assert_eq!(back(text, 12), 8);
        assert_eq!(back(text, 8), 7);
        // "/to" is one word-like segment with "/" as punctuation boundary
        assert_eq!(back(text, 7), 5);
        assert_eq!(back(text, 5), 4);
        assert_eq!(back(text, 4), 0);
    }

    #[test]
    fn cjk_mixed() {
        let text = "你好世界 test";
        // UTF-16 5 -> byte 13, UTF-16 2 -> byte 6.
        assert_eq!(back(text, text.len()), 13);
        // Intl.Segmenter treats each CJK char as a separate word-like segment
        assert_eq!(back(text, 13), 6);
        assert_eq!(back(text, 6), 0);
    }

    #[test]
    fn whitespace_at_boundaries() {
        assert_eq!(back("  hello  ", 9), 2);
        assert_eq!(back("  hello  ", 2), 0);
    }

    #[test]
    fn punctuation_run_foo_bar() {
        assert_eq!(back("foo...bar", 9), 6);
        assert_eq!(back("foo...bar", 6), 3);
        assert_eq!(back("foo...bar", 3), 0);
    }

    #[test]
    fn cursor_at_0_returns_0() {
        assert_eq!(back("hello", 0), 0);
    }
}

mod find_word_forward_tests {
    use super::*;

    #[test]
    fn basic_words_hello_world() {
        assert_eq!(fwd("hello world", 0), 5);
        assert_eq!(fwd("hello world", 5), 11);
    }

    #[test]
    fn dotted_foo_bar() {
        assert_eq!(fwd("foo.bar", 0), 3);
        assert_eq!(fwd("foo.bar", 3), 4);
        assert_eq!(fwd("foo.bar", 4), 7);
    }

    #[test]
    fn colon_foo_bar() {
        assert_eq!(fwd("foo:bar", 0), 3);
        assert_eq!(fwd("foo:bar", 3), 4);
        assert_eq!(fwd("foo:bar", 4), 7);
    }

    #[test]
    fn path_path_to_file() {
        let text = "path/to/file";
        assert_eq!(fwd(text, 0), 4);
        assert_eq!(fwd(text, 4), 5);
        assert_eq!(fwd(text, 5), 7);
        assert_eq!(fwd(text, 7), 8);
        assert_eq!(fwd(text, 8), 12);
    }

    #[test]
    fn cjk_mixed() {
        let text = "你好世界 test";
        let first_end = fwd(text, 0);
        // UTF-16 range (0, 4] -> bytes (0, 12].
        assert!(first_end > 0);
        assert!(first_end <= 12);
        // Walk to end
        let mut pos = 0;
        while pos < text.len() {
            let next = fwd(text, pos);
            if next == pos {
                break;
            }
            pos = next;
        }
        assert_eq!(pos, text.len());
    }

    #[test]
    fn whitespace_at_boundaries() {
        assert_eq!(fwd("  hello  ", 0), 7);
        assert_eq!(fwd("  hello  ", 7), 9);
    }

    #[test]
    fn punctuation_run_foo_bar() {
        assert_eq!(fwd("foo...bar", 0), 3);
        assert_eq!(fwd("foo...bar", 3), 6);
        assert_eq!(fwd("foo...bar", 6), 9);
    }

    #[test]
    fn cursor_at_end_returns_end() {
        assert_eq!(fwd("hello", 5), 5);
    }
}

mod atomic_segments {
    use super::*;

    const MARKER: &str = "[paste #1 +5 lines]";

    fn text() -> String {
        format!("hello {MARKER} world")
    }

    fn seg(segment: &str, index: usize, is_word_like: bool) -> WordSegment<'_> {
        WordSegment {
            segment,
            index,
            is_word_like,
        }
    }

    // The functions slice text before calling segment(), so each expected substring maps to
    // its pre-split segments (indices as given by senpi's fixture).
    fn segment_map(input: &str) -> Vec<WordSegment<'_>> {
        let text = text();
        let at = |start: usize, len: usize| &input[start..start + len];
        if input == text {
            vec![
                seg(at(0, 5), 0, true),
                seg(at(5, 1), 5, false),
                seg(at(6, MARKER.len()), 6, true),
                seg(at(25, 1), 25, false),
                seg(at(26, 5), 26, true),
            ]
        } else if input == &text[..26] {
            vec![
                seg(at(0, 5), 0, true),
                seg(at(5, 1), 5, false),
                seg(at(6, MARKER.len()), 6, true),
                seg(at(25, 1), 25, false),
            ]
        } else if input == &text[6..] {
            vec![
                seg(at(0, MARKER.len()), 0, true),
                seg(at(19, 1), 19, false),
                seg(at(20, 5), 20, true),
            ]
        } else {
            Vec::new()
        }
    }

    fn with_opts<R>(f: impl FnOnce(&WordNavigationOptions<'_>) -> R) -> R {
        let is_atomic = |s: &str| s == MARKER;
        let opts = WordNavigationOptions {
            segment: Some(&segment_map),
            is_atomic_segment: Some(&is_atomic),
        };
        f(&opts)
    }

    #[test]
    fn backward_skips_word_then_stops_before_atomic_marker() {
        let text = text();
        assert_eq!(with_opts(|o| find_word_backward(&text, text.len(), o)), 26);
    }

    #[test]
    fn backward_skips_whitespace_then_atomic_marker_as_one_unit() {
        let text = text();
        assert_eq!(with_opts(|o| find_word_backward(&text, 26, o)), 6);
    }

    #[test]
    fn forward_skips_atomic_marker_as_one_unit() {
        let text = text();
        assert_eq!(
            with_opts(|o| find_word_forward(&text, 6, o)),
            6 + MARKER.len()
        );
    }
}
