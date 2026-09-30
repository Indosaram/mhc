//! Ports of senpi's utils.ts test files, one nested module per TS file.

use regex::Regex;

use super::*;

mod wrap_ansi {
    //! Port of senpi packages/tui/test/wrap-ansi.test.ts.
    use super::*;

    const OUTER_BG: &str = "\x1b[48;2;40;50;40m";

    fn outer(text: &str) -> String {
        format!("{OUTER_BG}{text}\x1b[49m")
    }

    #[test]
    fn given_inner_background_reset_inside_outer_background_when_applying_line_background_then_outer_background_resumes()
     {
        let inner_bg = "\x1b[48;2;60;30;30m";
        let bg_reset = "\x1b[49m";
        let line = format!("left {inner_bg}row{bg_reset} tail");
        let rendered =
            apply_background_to_line(&line, 20, &|text| format!("{OUTER_BG}{text}{bg_reset}"));
        assert!(rendered.contains(&format!("{bg_reset}{OUTER_BG} tail")));
    }

    #[test]
    fn given_full_reset_inside_outer_background_when_applying_line_background_then_outer_background_resumes()
     {
        let reset = "\x1b[0m";
        let line = format!("left {reset}tail");
        let rendered = apply_background_to_line(&line, 20, &outer);
        assert!(rendered.contains(&format!("{reset}{OUTER_BG}tail")));
    }

    #[test]
    fn given_compound_background_reset_inside_outer_background_when_applying_line_background_then_outer_background_resumes()
     {
        let compound_reset = "\x1b[39;49m";
        let line = format!("left {compound_reset}tail");
        let rendered = apply_background_to_line(&line, 20, &outer);
        assert!(rendered.contains(&format!("{compound_reset}{OUTER_BG}tail")));
    }

    #[test]
    fn given_rgb_inner_background_contains_zero_channels_when_applying_line_background_then_inner_background_is_preserved()
     {
        let inner_bg = "\x1b[48;2;0;30;30m";
        let line = format!("{inner_bg}chip");
        let rendered = apply_background_to_line(&line, 8, &outer);
        assert!(rendered.contains(&format!("{inner_bg}chip")));
        assert!(!rendered.contains(&format!("{inner_bg}{OUTER_BG}chip")));
    }

    #[test]
    fn given_rgb_foreground_contains_zero_channels_inside_inner_background_when_applying_line_background_then_inner_background_is_preserved()
     {
        let inner_bg = "\x1b[48;2;60;30;30m";
        let foreground = "\x1b[38;2;0;200;0m";
        let line = format!("{inner_bg}{foreground}chip");
        let rendered = apply_background_to_line(&line, 8, &outer);
        assert!(rendered.contains(&format!("{foreground}chip")));
        assert!(!rendered.contains(&format!("{foreground}{OUTER_BG}chip")));
    }

    #[test]
    fn given_sgr_reset_also_sets_a_new_background_when_applying_line_background_then_new_background_is_preserved()
     {
        let reset_then_inner_bg = "\x1b[0;48;2;0;30;30m";
        let line = format!("{reset_then_inner_bg}chip");
        let rendered = apply_background_to_line(&line, 8, &outer);
        assert!(rendered.contains(&format!("{reset_then_inner_bg}chip")));
        assert!(!rendered.contains(&format!("{reset_then_inner_bg}{OUTER_BG}chip")));
    }

    #[test]
    fn given_unclosed_inner_background_reaches_line_padding_when_applying_line_background_then_padding_uses_outer_background()
     {
        let inner_bg = "\x1b[48;2;60;30;30m";
        let reset = "\x1b[0m";
        let line = format!("{inner_bg}tool");
        let rendered = apply_background_to_line(&line, 8, &outer);
        assert!(rendered.contains(&format!("{inner_bg}tool{reset}{OUTER_BG}    ")));
    }

    const UNDERLINE_ON: &str = "\x1b[4m";
    const UNDERLINE_OFF: &str = "\x1b[24m";

    #[test]
    fn should_not_apply_underline_style_before_the_styled_text() {
        let url = "https://example.com/very/long/path/that/will/wrap";
        let text = format!("read this thread {UNDERLINE_ON}{url}{UNDERLINE_OFF}");
        let wrapped = wrap_text_with_ansi(&text, 40);
        // First line should NOT contain underline code - it's just "read this thread"
        assert_eq!(wrapped[0], "read this thread");
        // Second line should start with underline, have URL content
        assert!(wrapped[1].starts_with(UNDERLINE_ON));
        assert!(wrapped[1].contains("https://"));
    }

    #[test]
    fn should_not_have_whitespace_before_underline_reset_code() {
        let text = format!("{UNDERLINE_ON}underlined text here {UNDERLINE_OFF}more");
        let wrapped = wrap_text_with_ansi(&text, 18);
        assert!(!wrapped[0].contains(&format!(" {UNDERLINE_OFF}")));
    }

    #[test]
    fn should_not_bleed_underline_to_padding_each_line_should_end_with_reset_for_underline_only() {
        let url = "https://example.com/very/long/path/that/will/definitely/wrap";
        let text = format!("prefix {UNDERLINE_ON}{url}{UNDERLINE_OFF} suffix");
        let wrapped = wrap_text_with_ansi(&text, 30);
        // Middle lines (with underlined content) should end with underline-off, not full reset
        for line in &wrapped[1..wrapped.len() - 1] {
            if line.contains(UNDERLINE_ON) {
                assert!(line.ends_with(UNDERLINE_OFF));
                assert!(!line.ends_with("\x1b[0m"));
            }
        }
    }

    #[test]
    fn should_preserve_background_color_across_wrapped_lines_without_full_reset() {
        let bg_blue = "\x1b[44m";
        let reset = "\x1b[0m";
        let text = format!("{bg_blue}hello world this is blue background text{reset}");
        let wrapped = wrap_text_with_ansi(&text, 15);
        for line in &wrapped {
            assert!(line.contains(bg_blue));
        }
        // Middle lines should NOT end with full reset (kills background for padding)
        for line in &wrapped[..wrapped.len() - 1] {
            assert!(!line.ends_with("\x1b[0m"));
        }
    }

    #[test]
    fn should_reset_underline_but_preserve_background_when_wrapping_underlined_text_inside_background()
     {
        let reset = "\x1b[0m";
        let text = format!(
            "\x1b[41mprefix {UNDERLINE_ON}UNDERLINED_CONTENT_THAT_WRAPS{UNDERLINE_OFF} suffix{reset}"
        );
        let wrapped = wrap_text_with_ansi(&text, 20);
        for line in &wrapped {
            assert!(line.contains("[41m") || line.contains(";41m") || line.contains("[41;"));
        }
        for line in &wrapped[..wrapped.len() - 1] {
            if (line.contains("[4m") || line.contains("[4;") || line.contains(";4m"))
                && !line.contains(UNDERLINE_OFF)
            {
                assert!(line.ends_with(UNDERLINE_OFF));
                assert!(!line.ends_with("\x1b[0m"));
            }
        }
    }

    #[test]
    fn should_handle_lf_crlf_and_cr_line_endings() {
        assert_eq!(
            wrap_text_with_ansi("first\nsecond\r\nthird\rfourth", 80),
            ["first", "second", "third", "fourth"]
        );
    }

    #[test]
    fn should_preserve_ansi_state_across_crlf_and_cr_line_endings() {
        let red = "\x1b[31m";
        let reset = "\x1b[0m";
        assert_eq!(
            wrap_text_with_ansi(&format!("{red}first\r\nsecond\rthird{reset}"), 80),
            [
                format!("{red}first"),
                format!("{red}second"),
                format!("{red}third{reset}")
            ]
        );
    }

    #[test]
    fn should_wrap_plain_text_correctly() {
        let wrapped = wrap_text_with_ansi("hello world this is a test", 10);
        assert!(wrapped.len() > 1);
        for line in &wrapped {
            assert!(visible_width(line) <= 10);
        }
    }

    #[test]
    fn should_break_cjk_runs_at_grapheme_boundaries_after_latin_text() {
        let text = "This is an example 中文汉字测试段落内容中文汉字测试段落内容.";
        let wrapped = wrap_text_with_ansi(text, 40);
        assert_eq!(
            wrapped,
            [
                "This is an example 中文汉字测试段落内容",
                "中文汉字测试段落内容."
            ]
        );
        for line in &wrapped {
            assert!(visible_width(line) <= 40);
        }
    }

    #[test]
    fn should_preserve_color_codes_when_wrapping_cjk_runs() {
        let red = "\x1b[31m";
        let reset = "\x1b[0m";
        let text =
            format!("{red}This is an example 中文汉字测试段落内容中文汉字测试段落内容.{reset}");
        let wrapped = wrap_text_with_ansi(&text, 40);
        assert_eq!(wrapped.len(), 2);
        assert_eq!(
            wrapped[0],
            format!("{red}This is an example 中文汉字测试段落内容")
        );
        assert_eq!(wrapped[1], format!("{red}中文汉字测试段落内容.{reset}"));
        for line in &wrapped {
            assert!(visible_width(line) <= 40);
        }
    }

    #[test]
    fn should_ignore_osc_133_semantic_markers_in_visible_width() {
        assert_eq!(visible_width("\x1b]133;A\x07hello\x1b]133;B\x07"), 5);
    }

    #[test]
    fn should_ignore_osc_sequences_terminated_with_st_in_visible_width() {
        assert_eq!(visible_width("\x1b]133;A\x1b\\hello\x1b]133;B\x1b\\"), 5);
    }

    #[test]
    fn should_treat_isolated_regional_indicators_as_width_2() {
        assert_eq!(visible_width("🇨"), 2);
        assert_eq!(visible_width("🇨🇳"), 2);
    }

    #[test]
    fn should_truncate_trailing_whitespace_that_exceeds_width() {
        let wrapped = wrap_text_with_ansi("  ", 1);
        assert!(visible_width(&wrapped[0]) <= 1);
    }

    #[test]
    fn should_preserve_color_codes_across_wraps() {
        let red = "\x1b[31m";
        let reset = "\x1b[0m";
        let wrapped = wrap_text_with_ansi(&format!("{red}hello world this is red{reset}"), 10);
        for line in &wrapped[1..] {
            assert!(line.starts_with(red));
        }
        for line in &wrapped[..wrapped.len() - 1] {
            assert!(!line.ends_with("\x1b[0m"));
        }
    }

    const URL: &str = "https://example.com";

    fn osc8_input() -> String {
        format!("\x1b]8;;{URL}\x1b\\0123456789\x1b]8;;\x1b\\")
    }

    #[test]
    fn re_emits_osc_8_open_at_the_start_of_continuation_lines() {
        let open = format!("\x1b]8;;{URL}\x1b\\");
        let osc8 = Regex::new(r"\x1b\]8;;[^\x1b\x07]*\x1b\\").expect("regex");
        let sgr = Regex::new(r"\x1b\[[0-9;]*m").expect("regex");
        for line in wrap_text_with_ansi(&osc8_input(), 6) {
            let stripped = sgr
                .replace_all(&osc8.replace_all(&line, ""), "")
                .into_owned();
            if !stripped.trim().is_empty() {
                assert!(
                    line.starts_with(&open) || line.contains(&open),
                    "Line {line:?} has visible text but no OSC 8 re-open"
                );
            }
        }
    }

    #[test]
    fn closes_osc_8_before_each_line_break() {
        let open = format!("\x1b]8;;{URL}\x1b\\");
        let lines = wrap_text_with_ansi(&osc8_input(), 6);
        for line in &lines[..lines.len() - 1] {
            if line.contains(&open) {
                assert!(
                    line.ends_with("\x1b]8;;\x1b\\"),
                    "Non-final line {line:?} is inside a hyperlink but does not close it"
                );
            }
        }
    }

    #[test]
    fn preserves_bel_terminators_when_wrapping_oauth_style_hyperlinks() {
        let url = format!("https://example.com/oauth/{}", "a".repeat(32));
        let input = format!("\x1b]8;;{url}\x07{url}\x1b]8;;\x07");
        let lines = wrap_text_with_ansi(&input, 20);
        assert!(lines.len() > 1);
        for line in &lines {
            assert!(
                line.contains(&format!("\x1b]8;;{url}\x07")),
                "Line {line:?} does not reopen the hyperlink with BEL"
            );
            assert!(
                !line.contains(&format!("\x1b]8;;{url}\x1b\\")),
                "Line {line:?} reopens the hyperlink with ST"
            );
        }
        for line in &lines[..lines.len() - 1] {
            assert!(
                line.ends_with("\x1b]8;;\x07"),
                "Line {line:?} does not close the hyperlink with BEL"
            );
        }
    }

    #[test]
    fn does_not_emit_osc_8_sequences_on_lines_that_are_outside_the_hyperlink() {
        let input = format!("before \x1b]8;;{URL}\x1b\\link\x1b]8;;\x1b\\ after");
        let lines = wrap_text_with_ansi(&input, 80);
        assert_eq!(lines.len(), 1);
        let open_count = Regex::new(r"\x1b\]8;;https:[^\x1b]+\x1b\\")
            .expect("regex")
            .find_iter(&lines[0])
            .count();
        let close_count = lines[0].matches("\x1b]8;;\x1b\\").count();
        assert_eq!(open_count, 1);
        assert_eq!(close_count, 1);
    }
}

mod truncate_to_width_tests {
    //! Port of senpi packages/tui/test/truncate-to-width.test.ts.
    use super::*;

    #[test]
    fn keeps_output_within_width_for_very_large_unicode_input() {
        let text = "🙂界".repeat(100_000);
        let truncated = truncate_to_width(&text, 40, "…", false);
        assert!(visible_width(&truncated) <= 40);
        assert!(truncated.ends_with("…\x1b[0m"));
    }

    #[test]
    fn preserves_ansi_styling_for_kept_text_and_resets_before_and_after_ellipsis() {
        let text = format!("\x1b[31m{}\x1b[0m", "hello ".repeat(1000));
        let truncated = truncate_to_width(&text, 20, "…", false);
        assert!(visible_width(&truncated) <= 20);
        assert!(truncated.contains("\x1b[31m"));
        assert!(truncated.ends_with("\x1b[0m…\x1b[0m"));
    }

    #[test]
    fn closes_a_bel_terminated_osc_8_link_when_truncating_its_label() {
        let open = "\x1b]8;;https://example.com\x07";
        let close = "\x1b]8;;\x07";
        let text = format!("{open}some-longer-label-here{close}");
        assert_eq!(
            truncate_to_width(&text, 15, "...", false),
            format!("{open}some-longer-{close}\x1b[0m...\x1b[0m")
        );
    }

    #[test]
    fn handles_malformed_ansi_escape_prefixes_without_hanging() {
        let text = format!("abc\x1bnot-ansi {}", "🙂".repeat(1000));
        assert!(visible_width(&truncate_to_width(&text, 20, "…", false)) <= 20);
    }

    #[test]
    fn clips_wide_ellipsis_safely_and_brackets_it_with_resets() {
        assert_eq!(truncate_to_width("abcdef", 1, "🙂", false), "");
        assert_eq!(
            truncate_to_width("abcdef", 2, "🙂", false),
            "\x1b[0m🙂\x1b[0m"
        );
        assert!(visible_width(&truncate_to_width("abcdef", 2, "🙂", false)) <= 2);
    }

    #[test]
    fn returns_the_original_text_when_it_already_fits_even_if_ellipsis_is_too_wide() {
        assert_eq!(truncate_to_width("a", 2, "🙂", false), "a");
        assert_eq!(truncate_to_width("界", 2, "🙂", false), "界");
    }

    #[test]
    fn pads_truncated_output_to_requested_width() {
        assert_eq!(
            visible_width(&truncate_to_width("🙂界🙂界🙂界", 8, "…", true)),
            8
        );
    }

    #[test]
    fn adds_a_trailing_reset_when_truncating_without_an_ellipsis() {
        let truncated =
            truncate_to_width(&format!("\x1b[31m{}", "hello".repeat(100)), 10, "", false);
        assert!(visible_width(&truncated) <= 10);
        assert!(truncated.ends_with("\x1b[0m"));
    }

    #[test]
    fn keeps_a_contiguous_prefix_instead_of_skipping_a_wide_grapheme_and_resuming_later() {
        assert_eq!(
            truncate_to_width("🙂\t界 \x1b_abc\x07", 7, "…", true),
            "🙂\t\x1b[0m…\x1b[0m "
        );
    }

    #[test]
    fn counts_tabs_inline_and_skips_ansi_inline() {
        assert_eq!(visible_width("\t\x1b[31m界\x1b[0m"), 5);
    }

    #[test]
    fn counts_indic_conjunct_spacing_code_points_within_grapheme_clusters() {
        assert_eq!(visible_width("र्क"), 2);
        assert_eq!(visible_width("नेटवर्क"), 5);
        assert_eq!(visible_width("सर्वाधिकार सुरक्षित। ऑर्डर पर क्लिक करें"), 33);
        assert_eq!(visible_width("র্ক"), 2);
        assert_eq!(visible_width("ર્ક"), 2);
        assert_eq!(visible_width("ର୍କ"), 2);
        assert_eq!(visible_width("ర్క"), 2);
        assert_eq!(visible_width("ര്‍ക"), 2);
    }

    #[test]
    fn keeps_ordinary_combining_marks_zero_width() {
        assert_eq!(visible_width("e\u{0301}"), 1);
        assert_eq!(visible_width("čřžůú"), 5);
        assert_eq!(visible_width("שָׁ"), 1);
        assert_eq!(visible_width("بّ"), 1);
        assert_eq!(visible_width("རྐ"), 1);
        assert_eq!(visible_width("ᜠ᜴"), 1);
        assert_eq!(visible_width("가〮"), 2);
        assert_eq!(visible_width("가〯"), 2);
    }

    #[test]
    fn keeps_cjk_and_japanese_width_accounting_unchanged() {
        assert_eq!(visible_width("网络"), 4);
        assert_eq!(visible_width("ネットワーク"), 12);
        assert_eq!(visible_width("が"), 2);
        assert_eq!(visible_width("か\u{3099}"), 2);
    }

    #[test]
    fn counts_myanmar_marks_that_terminals_allocate_cells_for() {
        for s in ["ကာ", "ကေ", "က်", "ကျ", "ကြ", "ကဳ", "ကဴ", "ကဵ", "ကး"]
        {
            assert_eq!(visible_width(s), 2, "{s}");
        }
        assert_eq!(visible_width("ကို"), 1);
        assert_eq!(visible_width("က္"), 1);
    }

    #[test]
    fn keeps_thai_and_lao_am_clusters_at_their_normal_cell_width() {
        assert_eq!(visible_width("ำ"), 1);
        assert_eq!(visible_width("ຳ"), 1);
        assert_eq!(visible_width("กำ"), 2);
        assert_eq!(visible_width("ກຳ"), 2);
    }

    #[test]
    fn normalizes_thai_and_lao_am_vowels_only_for_terminal_output() {
        assert_eq!(normalize_terminal_output("ำ"), "ํา");
        assert_eq!(normalize_terminal_output("ຳ"), "ໍາ");
        assert_eq!(
            visible_width(&normalize_terminal_output("ำabc")),
            visible_width("ำabc")
        );
        assert_eq!(
            visible_width(&normalize_terminal_output("ຳabc")),
            visible_width("ຳabc")
        );
    }
}

mod tab_width {
    //! Port of senpi packages/tui/test/tab-width.test.ts. The fourth case ("keeps tab-containing
    //! overlays on one physical terminal row") drives TuiMainScreen and ports with todo 7.
    use super::*;

    const TEXT: &str = "out 192M\t.pi/skill-tests/results-ha";

    #[test]
    fn keeps_slice_helper_widths_consistent_with_visible_width() {
        let (text, width) = slice_with_width(TEXT, 0, 10, true);
        assert_eq!(text, "out 192M");
        assert_eq!(width, 8);
        assert_eq!(visible_width(&text), width);
    }

    #[test]
    fn keeps_overlay_segment_widths_consistent_with_visible_width() {
        let segments = extract_segments(TEXT, 10, 13, 10, true);
        assert_eq!(segments.before, "out 192M");
        assert_eq!(segments.before_width, 8);
        assert_eq!(visible_width(&segments.before), segments.before_width);

        let tab_fits = extract_segments(TEXT, 11, 13, 10, true);
        assert_eq!(tab_fits.before, "out 192M\t");
        assert_eq!(tab_fits.before_width, 11);
        assert_eq!(visible_width(&tab_fits.before), tab_fits.before_width);
    }

    #[test]
    fn keeps_tabs_inside_terminal_control_sequences_byte_identical() {
        for control in [
            "\x1b]8;;https://example.test/a\tb\x07",
            "\x1b]0;window\ttitle\x1b\\",
            "\x1b_payload\tdata\x1b\\",
        ] {
            assert_eq!(
                normalize_terminal_output(&format!("{control}label\ttext")),
                format!("{control}label   text")
            );
        }
    }
}

mod width_cache {
    //! Port of senpi packages/tui/test/width-cache.test.ts. The cache is per-thread in Rust, so
    //! each test observes only its own traffic.
    use super::*;
    use crate::process_env::with_overrides;

    fn styled_key(prefix: &str, index: usize) -> String {
        format!("\x1b[31m{prefix}-{index}-汉字\x1b[0m")
    }

    fn stats() -> WidthCacheStats {
        with_overrides(&[("PI_TUI_TEST_SEAMS", Some("1"))], __width_cache_stats)
            .expect("width cache stats seam must be enabled")
    }

    #[test]
    fn keeps_rotated_keys_retrievable_from_the_previous_generation() {
        let keys: Vec<String> = (0..2049).map(|i| styled_key("generation", i)).collect();
        for key in &keys {
            visible_width(key);
        }
        let after_rotation = stats();
        let first_width = visible_width(&keys[0]);
        let after_probe = stats();
        assert_eq!(first_width, 17);
        assert_eq!(after_probe.hits, after_rotation.hits + 1);
        assert!(after_rotation.total_retained <= 4096);
        assert!(after_probe.total_retained <= 4096);
    }

    #[test]
    fn returns_identical_widths_for_a_50_case_corpus_before_and_after_rotation() {
        let mut corpus: Vec<String> = Vec::new();
        corpus.extend((0..10).map(|i| format!("ascii-{i}-plain")));
        corpus.extend((0..10).map(|i| format!("汉字-{i}-中文")));
        corpus.extend((0..10).map(|i| format!("emoji-{i}-👨‍💻-🏳️‍🌈")));
        corpus.extend((0..10).map(|i| format!("\x1b[3{}mansi-{i}-中\x1b[0m", i % 8)));
        corpus.extend(
            (0..10)
                .map(|i| format!("\x1b]8;;https://example.com/{i}\x1b\\osc-{i}-界\x1b]8;;\x1b\\")),
        );
        assert_eq!(corpus.len(), 50);
        let before: Vec<usize> = corpus.iter().map(|e| visible_width(e)).collect();
        for i in 0..5000 {
            visible_width(&styled_key("rotation-filler", i));
        }
        let after: Vec<usize> = corpus.iter().map(|e| visible_width(e)).collect();
        assert_eq!(after, before);
    }

    #[test]
    fn keeps_at_least_half_of_repeated_3000_key_cyclic_sweeps_on_cache_hits() {
        let keys: Vec<String> = (0..3000).map(|i| styled_key("cyclic", i)).collect();
        for key in &keys {
            visible_width(key);
        }
        let before = stats();
        for _ in 0..10 {
            for key in &keys {
                visible_width(key);
            }
        }
        let after = stats();
        let hits = after.hits - before.hits;
        let misses = after.misses - before.misses;
        assert!(hits + misses > 0);
        assert!(
            hits * 2 >= hits + misses,
            "expected hit rate >= 0.5, got {hits}/{}",
            hits + misses
        );
        assert!(after.total_retained <= 4096);
    }
}

mod regression_regional_indicator_width {
    //! Port of senpi packages/tui/test/regression-regional-indicator-width.test.ts.
    use super::*;

    #[test]
    fn treats_partial_flag_grapheme_as_full_width_to_avoid_streaming_render_drift() {
        assert_eq!(visible_width("🇨"), 2);
        assert_eq!(visible_width("      - 🇨"), 10);
    }

    #[test]
    fn wraps_intermediate_partial_flag_list_line_before_overflow() {
        let wrapped = wrap_text_with_ansi("      - 🇨", 9);
        assert_eq!(wrapped.len(), 2);
        assert_eq!(visible_width(&wrapped[0]), 7);
        assert_eq!(visible_width(&wrapped[1]), 2);
    }

    #[test]
    fn treats_all_regional_indicator_singleton_graphemes_as_width_2() {
        for cp in 0x1f1e6..=0x1f1ff {
            let ri = char::from_u32(cp).expect("regional indicator").to_string();
            assert_eq!(
                visible_width(&ri),
                2,
                "Expected {ri} (U+{cp:X}) to be width 2"
            );
        }
    }

    #[test]
    fn keeps_full_flag_pairs_at_width_2() {
        for flag in ["🇯🇵", "🇺🇸", "🇬🇧", "🇨🇳", "🇩🇪", "🇫🇷"] {
            assert_eq!(visible_width(flag), 2, "Expected {flag} to be width 2");
        }
    }

    #[test]
    fn keeps_common_streaming_emoji_intermediates_at_stable_width() {
        for sample in ["👍", "👍🏻", "✅", "⚡", "⚡️", "👨", "👨‍💻", "🏳️‍🌈"]
        {
            assert_eq!(visible_width(sample), 2, "Expected {sample} to be width 2");
        }
    }
}

mod regression_overlay_cjk_boundary {
    //! Port of the utils-level cases of senpi packages/tui/test/regression-overlay-cjk-boundary.test.ts;
    //! the two compositeTuiLine cases port with tui.ts (todo 7).
    use super::*;

    #[test]
    fn excludes_a_wide_grapheme_from_before_when_overlay_starts_inside_it() {
        let segments = extract_segments("abcd让EFGH", 5, 9, 11, true);
        assert_eq!(segments.before, "abcd");
        assert_eq!(segments.before_width, 4);
        assert_eq!(visible_width(&segments.before), segments.before_width);
        assert_eq!(segments.after, "H");
        assert_eq!(segments.after_width, 1);
    }

    #[test]
    fn keeps_ascii_before_segment_behavior_at_the_same_boundary() {
        let segments = extract_segments("abcdG EFGH", 5, 9, 11, true);
        assert_eq!(segments.before, "abcdG");
        assert_eq!(segments.before_width, 5);
        assert_eq!(visible_width(&segments.before), segments.before_width);
    }
}

mod segmenter_exports {
    //! Port of senpi packages/tui/test/segmenter-exports.test.ts. Rust exposes the shared
    //! segmenters as the \`graphemes\` / \`word_segments\` functions from the crate root.

    #[test]
    fn re_exports_the_shared_grapheme_segmenter() {
        assert_eq!(
            crate::utils::graphemes("a👍🏽b").count(),
            3,
            "emoji modifier sequence must stay one cluster"
        );
    }

    #[test]
    fn re_exports_the_shared_word_segmenter() {
        let words: Vec<(&str, bool)> = crate::utils::word_segments("hello world")
            .iter()
            .map(|s| (s.segment, s.is_word_like))
            .collect();
        assert_eq!(words, [("hello", true), (" ", false), ("world", true)]);
    }
}
