//! Port of senpi packages/ai/src/tool-call-middleware/recovery-code-mask.ts.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryCodeMaskSegment {
    pub text: String,
    pub scan: bool,
    pub recovery_boundary: bool,
}

#[derive(Default)]
pub struct RecoveryCodeMaskFeedOptions {
    pub active_invoke: bool,
}

#[derive(Clone)]
enum MaskState {
    Plain,
    Inline { delimiter_length: usize },
    Fenced { delimiter_length: usize, closing_line: bool },
}

pub struct RecoveryCodeMask {
    state: MaskState,
    at_line_start: bool,
    leading_spaces: usize,
    plain_indent: String,
    pending_tick_count: usize,
    pending_deferred_ticks: usize,
    pending_at_line_start: bool,
    finished: bool,
}

fn emit(segments: &mut Vec<RecoveryCodeMaskSegment>, text: &str, scan: bool, recovery_boundary: bool) {
    if text.is_empty() {
        return;
    }
    if let Some(previous) = segments.last_mut()
        && previous.scan == scan
        && !previous.recovery_boundary
        && !recovery_boundary
    {
        previous.text.push_str(text);
        return;
    }
    segments.push(RecoveryCodeMaskSegment { text: text.to_string(), scan, recovery_boundary });
}

fn is_line_break(character: char) -> bool {
    character == '\r' || character == '\n'
}

impl RecoveryCodeMask {
    fn flush_plain_indent(&mut self, segments: &mut Vec<RecoveryCodeMaskSegment>, scan: bool, recovery_boundary: bool) {
        let indent = std::mem::take(&mut self.plain_indent);
        emit(segments, &indent, scan, recovery_boundary);
    }

    fn update_line_position(&mut self, character: char) {
        if is_line_break(character) {
            self.at_line_start = true;
            self.leading_spaces = 0;
        } else {
            self.at_line_start = false;
        }
    }

    fn complete_pending_ticks(&mut self, segments: &mut Vec<RecoveryCodeMaskSegment>) {
        if self.pending_tick_count == 0 {
            return;
        }
        let tick_count = self.pending_tick_count;
        let started_at_line_start = self.pending_at_line_start;
        self.pending_tick_count = 0;
        self.pending_at_line_start = false;

        match &self.state {
            MaskState::Plain => {
                if started_at_line_start && tick_count >= 3 {
                    self.flush_plain_indent(segments, false, true);
                    self.state = MaskState::Fenced { delimiter_length: tick_count, closing_line: false };
                } else {
                    self.flush_plain_indent(segments, true, false);
                    if self.pending_deferred_ticks > 0 {
                        emit(segments, &"`".repeat(self.pending_deferred_ticks), false, true);
                    }
                    self.state = MaskState::Inline { delimiter_length: tick_count };
                }
            }
            MaskState::Inline { delimiter_length } => {
                if tick_count == *delimiter_length {
                    self.state = MaskState::Plain;
                }
            }
            MaskState::Fenced { delimiter_length, .. } => {
                if started_at_line_start && tick_count >= *delimiter_length {
                    self.state = MaskState::Fenced { delimiter_length: *delimiter_length, closing_line: true };
                }
            }
        }
        self.pending_deferred_ticks = 0;
        self.at_line_start = false;
        self.leading_spaces = 0;
    }

    fn append_ticks(&mut self, segments: &mut Vec<RecoveryCodeMaskSegment>, ticks: &str) {
        if self.pending_tick_count == 0 {
            self.pending_at_line_start = self.at_line_start;
        }
        self.pending_tick_count += ticks.len();
        let mut offset = 0usize;
        if matches!(self.state, MaskState::Plain) && self.pending_at_line_start && !self.plain_indent.is_empty() && self.pending_deferred_ticks < 3 {
            let deferred = (3 - self.pending_deferred_ticks).min(ticks.len());
            self.pending_deferred_ticks += deferred;
            offset = deferred;
            if self.pending_deferred_ticks == 3 {
                self.flush_plain_indent(segments, false, true);
                emit(segments, "```", false, false);
                self.pending_deferred_ticks = 0;
            }
        }
        if offset < ticks.len() {
            let recovery_boundary = matches!(self.state, MaskState::Plain) && self.pending_tick_count == ticks.len();
            emit(segments, &ticks[offset..], false, recovery_boundary);
        }
    }

    fn process_plain_character(&mut self, segments: &mut Vec<RecoveryCodeMaskSegment>, character: char) {
        if self.at_line_start && character == ' ' && self.leading_spaces < 3 {
            self.plain_indent.push(character);
            self.leading_spaces += 1;
            return;
        }
        self.flush_plain_indent(segments, true, false);
        emit(segments, &character.to_string(), true, false);
        self.update_line_position(character);
    }

    fn process_inline_character(&mut self, segments: &mut Vec<RecoveryCodeMaskSegment>, character: char) {
        emit(segments, &character.to_string(), false, false);
        if is_line_break(character) {
            self.state = MaskState::Plain;
        }
        self.update_line_position(character);
    }

    fn process_fenced_character(&mut self, segments: &mut Vec<RecoveryCodeMaskSegment>, character: char) {
        let MaskState::Fenced { closing_line, .. } = &self.state else { return };
        let closing_line = *closing_line;
        if self.at_line_start && character == ' ' && self.leading_spaces < 3 {
            emit(segments, &character.to_string(), false, false);
            self.leading_spaces += 1;
            return;
        }
        emit(segments, &character.to_string(), false, false);
        if closing_line && is_line_break(character) {
            self.state = MaskState::Plain;
        }
        self.update_line_position(character);
    }

    fn process_text(&mut self, segments: &mut Vec<RecoveryCodeMaskSegment>, text: &str) {
        let chars: Vec<char> = text.chars().collect();
        let mut index = 0usize;
        while index < chars.len() {
            if chars[index] == '`' {
                let mut end = index + 1;
                while end < chars.len() && chars[end] == '`' {
                    end += 1;
                }
                let ticks: String = chars[index..end].iter().collect();
                self.append_ticks(segments, &ticks);
                index = end;
                continue;
            }
            self.complete_pending_ticks(segments);
            let character = chars[index];
            match self.state {
                MaskState::Plain => self.process_plain_character(segments, character),
                MaskState::Inline { .. } => self.process_inline_character(segments, character),
                MaskState::Fenced { .. } => self.process_fenced_character(segments, character),
            }
            index += 1;
        }
    }

    fn track_active_text(&mut self, text: &str) {
        for character in text.chars() {
            self.update_line_position(character);
        }
    }

    pub fn feed(&mut self, text: &str, options: Option<RecoveryCodeMaskFeedOptions>) -> Vec<RecoveryCodeMaskSegment> {
        assert!(!self.finished, "Recovery code mask is finished");
        let mut segments = Vec::new();
        if options.is_some_and(|options| options.active_invoke) {
            self.complete_pending_ticks(&mut segments);
            self.flush_plain_indent(&mut segments, true, false);
            emit(&mut segments, text, true, false);
            self.track_active_text(text);
        } else {
            self.process_text(&mut segments, text);
        }
        segments
    }

    pub fn finish(&mut self) -> Vec<RecoveryCodeMaskSegment> {
        if self.finished {
            return Vec::new();
        }
        let mut segments = Vec::new();
        self.complete_pending_ticks(&mut segments);
        if matches!(self.state, MaskState::Plain) {
            self.flush_plain_indent(&mut segments, true, false);
        }
        self.finished = true;
        segments
    }
}

pub fn create_recovery_code_mask() -> RecoveryCodeMask {
    RecoveryCodeMask {
        state: MaskState::Plain,
        at_line_start: true,
        leading_spaces: 0,
        plain_indent: String::new(),
        pending_tick_count: 0,
        pending_deferred_ticks: 0,
        pending_at_line_start: false,
        finished: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_call_middleware::types::StreamParserEvent;
    use crate::types::Tool;
    use serde_json::{json, Value};

    fn scan_text(segments: &[RecoveryCodeMaskSegment]) -> String {
        segments.iter().filter(|segment| segment.scan).map(|segment| segment.text.as_str()).collect()
    }

    #[test]
    fn plain_text_is_all_scannable() {
        let mut mask = create_recovery_code_mask();
        let mut segments = mask.feed("hello world", None);
        segments.extend(mask.finish());
        assert_eq!(scan_text(&segments), "hello world");
    }

    #[test]
    fn inline_backtick_span_is_masked_from_scanning() {
        let mut mask = create_recovery_code_mask();
        let mut segments = mask.feed("before `code` after", None);
        segments.extend(mask.finish());
        assert_eq!(scan_text(&segments), "before  after");
        let full: String = segments.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(full, "before `code` after");
    }

    #[test]
    fn fenced_code_block_is_masked_until_closing_fence() {
        let mut mask = create_recovery_code_mask();
        let mut segments = mask.feed("```\ncode line\n```\nafter", None);
        segments.extend(mask.finish());
        assert_eq!(scan_text(&segments), "after");
    }

    #[test]
    fn active_invoke_option_marks_everything_scannable_without_masking() {
        let mut mask = create_recovery_code_mask();
        let segments = mask.feed("`still scanned`", Some(RecoveryCodeMaskFeedOptions { active_invoke: true }));
        assert_eq!(scan_text(&segments), "`still scanned`");
    }

    #[test]
    #[should_panic(expected = "Recovery code mask is finished")]
    fn feed_after_finish_panics() {
        let mut mask = create_recovery_code_mask();
        mask.finish();
        mask.feed("x", None);
    }
    fn bash_tool() -> Tool {
        Tool {
            name: "Bash".into(),
            description: "Run a command".into(),
            parameters: json!({"type": "object", "required": ["command"], "properties": {"command": {"type": "string", "minLength": 3}}}),
            freeform: None,
            constrained_sampling: None,
        }
    }

    fn code_invoke() -> &'static str {
        r#"<invoke name="Bash"><parameter name="command">echo example</parameter></invoke>"#
    }

    fn executable_invoke() -> &'static str {
        r#"<invoke name="Bash"><parameter name="command">echo executable</parameter></invoke>"#
    }

    struct MaskRun {
        text: String,
        events: Vec<StreamParserEvent>,
    }

    /// The JS test enumerates `[text]`, `[...text]` and every two-way split. Rust strings are
    /// UTF-8, so the per-code-unit spread becomes a per-char split; the two-way splits are the same.
    fn all_meaningful_chunk_splits(text: &str) -> Vec<Vec<String>> {
        let chars: Vec<char> = text.chars().collect();
        let mut splits = vec![vec![text.to_string()]];
        splits.push(chars.iter().map(|c| c.to_string()).collect());
        for index in 1..chars.len() {
            let head: String = chars[..index].iter().collect();
            let tail: String = chars[index..].iter().collect();
            splits.push(vec![head, tail]);
        }
        splits
    }

    fn run_mask(chunks: &[String]) -> MaskRun {
        let mut mask = create_recovery_code_mask();
        let mut parser = crate::tool_call_middleware::protocols::antml::recovery_stream::create_antml_invoke_recovery_stream_parser(vec![bash_tool()], None);
        let mut events = Vec::new();
        let mut text = String::new();

        for chunk in chunks {
            for segment in mask.feed(chunk, None) {
                text.push_str(&segment.text);
                if segment.recovery_boundary {
                    events.extend(parser.interrupt());
                }
                if segment.scan {
                    events.extend(parser.feed(&segment.text));
                }
            }
        }
        for segment in mask.finish() {
            text.push_str(&segment.text);
            if segment.recovery_boundary {
                events.extend(parser.interrupt());
            }
            if segment.scan {
                events.extend(parser.feed(&segment.text));
            }
        }
        events.extend(parser.finish());
        MaskRun { text, events }
    }

    fn recovered_commands(events: &[StreamParserEvent]) -> Vec<String> {
        events
            .iter()
            .filter_map(|event| match event {
                StreamParserEvent::ToolcallEnd { arguments, .. } => arguments.get("command").and_then(Value::as_str).map(str::to_string),
                _ => None,
            })
            .collect()
    }

    fn expect_across_every_split(input: &str, expected_commands: &[&str]) {
        for (index, chunks) in all_meaningful_chunk_splits(input).into_iter().enumerate() {
            let result = run_mask(&chunks);
            assert_eq!(result.text, input, "split {index} must preserve output");
            assert_eq!(recovered_commands(&result.events), expected_commands, "split {index} must recover only executable invokes");
        }
    }

    #[test]
    fn suppresses_invoke_like_examples_inside_code_while_preserving_later_executable_calls() {
        let input = format!("Example: `{}`\nThen run {}", code_invoke(), executable_invoke());
        expect_across_every_split(&input, &["echo executable"]);
    }

    #[test]
    fn masks_inline_and_fenced_invoke_examples_across_split_backtick_runs() {
        let inline_with_matching_delimiter = format!("Inline: ``{}`` then {}", code_invoke(), executable_invoke());
        let inline_newline_reset = format!("Unclosed: `{}\nThen {}", code_invoke(), executable_invoke());
        let mismatched_inline_close = format!("Inline: ``{}` still code\nThen {}", code_invoke(), executable_invoke());
        let indented_four_backtick_fence = format!("   ````xml\n{}\n```\n{}\n   ````\nThen {}", code_invoke(), code_invoke(), executable_invoke());

        expect_across_every_split(&inline_with_matching_delimiter, &["echo executable"]);
        expect_across_every_split(&inline_newline_reset, &["echo executable"]);
        expect_across_every_split(&mismatched_inline_close, &["echo executable"]);
        expect_across_every_split(&indented_four_backtick_fence, &["echo executable"]);
        for indent in ["", " ", "  ", "   "] {
            let input = format!("{indent}```xml\n{}\n{indent}```\nThen {}", code_invoke(), executable_invoke());
            expect_across_every_split(&input, &["echo executable"]);
        }
    }

    #[test]
    fn preserves_ordinary_text_and_active_call_backticks_across_every_split_point() {
        let active_call = r#"<invoke name="Bash"><parameter name="command">echo ```literal```</parameter></invoke>"#;
        for (index, chunks) in all_meaningful_chunk_splits(active_call).into_iter().enumerate() {
            let mut mask = create_recovery_code_mask();
            let mut parser = crate::tool_call_middleware::protocols::antml::recovery_stream::create_antml_invoke_recovery_stream_parser(vec![bash_tool()], None);
            let mut events = Vec::new();
            for chunk in &chunks {
                for segment in mask.feed(chunk, Some(RecoveryCodeMaskFeedOptions { active_invoke: true })) {
                    events.extend(parser.feed(&segment.text));
                }
            }
            events.extend(parser.finish());
            assert_eq!(recovered_commands(&events), vec!["echo ```literal```".to_string()], "active split {index}");
        }
    }

    #[test]
    fn preserves_ordinary_prose_and_invokes_split_across_every_boundary() {
        let input = format!("ordinary prose before {} ordinary prose after", executable_invoke());
        expect_across_every_split(&input, &["echo executable"]);
    }

    #[test]
    fn masked_spans_break_partial_recovery_candidates() {
        let bridged_invoke = r#"<inv`masked`oke name="Bash"><parameter name="command">echo bridged</parameter></invoke>"#;
        let mut mask = create_recovery_code_mask();
        let mut parser = crate::tool_call_middleware::protocols::antml::recovery_stream::create_antml_invoke_recovery_stream_parser(vec![bash_tool()], None);
        let mut events = Vec::new();
        let prefix = "<inv`masked`oke";
        let chunks = vec![
            "emoji \u{1F600} <inv".to_string(),
            String::new(),
            "`masked`oke".to_string(),
            bridged_invoke[prefix.len()..].to_string(),
            format!("\n{}", executable_invoke()),
        ];
        let mut output = String::new();

        for chunk in &chunks {
            for segment in mask.feed(chunk, None) {
                output.push_str(&segment.text);
                if segment.recovery_boundary {
                    events.extend(parser.interrupt());
                }
                if segment.scan {
                    events.extend(parser.feed(&segment.text));
                }
            }
        }
        for segment in mask.finish() {
            output.push_str(&segment.text);
            if segment.scan {
                events.extend(parser.feed(&segment.text));
            }
        }
        events.extend(parser.finish());

        assert_eq!(output, chunks.concat());
        assert_eq!(recovered_commands(&events), vec!["echo executable".to_string()]);
    }

    #[test]
    fn accepts_a_longer_matching_fence_closer() {
        let input = format!("```xml\n{}\n````\n{}", code_invoke(), executable_invoke());
        expect_across_every_split(&input, &["echo executable"]);
    }

    #[test]
    fn resets_an_unclosed_inline_span_on_cr_only_newline() {
        for newline in ["\r", "\r\n"] {
            let input = format!("\u{1F600} `{}{}{}", code_invoke(), newline, executable_invoke());
            expect_across_every_split(&input, &["echo executable"]);
        }
    }

    #[test]
    fn preserves_order_and_line_state_through_active_invoke_bypass() {
        let mut mask = create_recovery_code_mask();
        let mut ordered: Vec<RecoveryCodeMaskSegment> = mask.feed("`", None);
        ordered.extend(mask.feed("", Some(RecoveryCodeMaskFeedOptions { active_invoke: true })));
        ordered.extend(mask.feed("ABC\u{1F600}\r", Some(RecoveryCodeMaskFeedOptions { active_invoke: true })));
        ordered.extend(mask.finish());
        assert_eq!(ordered.iter().map(|segment| segment.text.as_str()).collect::<String>(), "`ABC\u{1F600}\r");

        let input = format!("x\r```xml\n{}\n```\n{}", code_invoke(), executable_invoke());
        let result = run_mask_with_active_prefix(&input);
        assert_eq!(result.text, input);
        assert_eq!(recovered_commands(&result.events), vec!["echo executable".to_string()]);
    }

    fn run_mask_with_active_prefix(input: &str) -> MaskRun {
        let mut mask = create_recovery_code_mask();
        let mut parser = crate::tool_call_middleware::protocols::antml::recovery_stream::create_antml_invoke_recovery_stream_parser(vec![bash_tool()], None);
        let mut events = Vec::new();
        let mut text = String::new();

        for segment in mask.feed("x", None) {
            text.push_str(&segment.text);
            if segment.scan {
                events.extend(parser.feed(&segment.text));
            }
        }
        for segment in mask.feed("\r", Some(RecoveryCodeMaskFeedOptions { active_invoke: true })) {
            text.push_str(&segment.text);
            if segment.scan {
                events.extend(parser.feed(&segment.text));
            }
        }
        let chars: Vec<char> = input.chars().collect();
        let rest: String = chars[2..].iter().collect();
        for segment in mask.feed(&rest, None) {
            text.push_str(&segment.text);
            if segment.scan {
                events.extend(parser.feed(&segment.text));
            }
        }
        for segment in mask.finish() {
            text.push_str(&segment.text);
            if segment.scan {
                events.extend(parser.feed(&segment.text));
            }
        }
        events.extend(parser.finish());
        MaskRun { text, events }
    }

    #[test]
    fn bounds_arbitrarily_long_backtick_runs() {
        let ticks = "`".repeat(1_000_000);
        let mut mask = create_recovery_code_mask();
        let mut during_feed = mask.feed(&ticks[..500_000], None);
        during_feed.extend(mask.feed("", None));
        during_feed.extend(mask.feed(&ticks[500_000..], None));
        let at_finish = mask.finish();

        assert_eq!(during_feed.iter().map(|segment| segment.text.as_str()).collect::<String>(), ticks);
        assert!(during_feed.iter().all(|segment| !segment.scan));
        assert!(at_finish.is_empty());
    }

    #[test]
    #[should_panic(expected = "Recovery code mask is finished")]
    fn rejects_feed_after_finish() {
        let mut mask = create_recovery_code_mask();
        mask.finish();
        assert!(mask.finish().is_empty());
        mask.feed("later", None);
    }

    fn mask_output(chunks: &[String], active_invoke: bool) -> String {
        let mut mask = create_recovery_code_mask();
        let mut segments = Vec::new();
        for chunk in chunks {
            let options = active_invoke.then_some(RecoveryCodeMaskFeedOptions { active_invoke: true });
            segments.extend(mask.feed(chunk, options));
        }
        segments.extend(mask.finish());
        segments.iter().map(|segment| segment.text.as_str()).collect()
    }

    fn char_split(text: &str, index: usize) -> Vec<String> {
        let chars: Vec<char> = text.chars().collect();
        vec![chars[..index].iter().collect(), String::new(), chars[index..].iter().collect()]
    }

    #[test]
    fn preserves_split_surrogate_pairs_across_repeated_fresh_masks() {
        // senpi splits on UTF-16 code units, which can land inside an emoji; Rust strings are UTF-8,
        // so the equivalent split is taken at the char boundary before the emoji.
        let input = "```xml meta=\u{1F600}\nX\n```\nY";
        let emoji_index = input.chars().position(|c| c == '\u{1F600}').expect("emoji present");
        for _ in 0..200 {
            assert_eq!(mask_output(&char_split(input, emoji_index), false), input);
        }
    }

    #[test]
    fn preserves_emoji_across_every_utf8_split_around_code_boundaries() {
        let cases: [(&str, bool); 5] = [
            ("```xml meta=\u{1F600}\nX\n```\nY", false),
            ("\u{1F600}`\u{1F600}`\u{1F600}", false),
            ("```\u{1F600}\r\n\u{1F600}\n```\r\u{1F600}", false),
            ("ordinary `\u{1F600} ordinary \u{1F600}` text", false),
            ("\u{1F600}```\u{1F600}\r\n\u{1F600}```\r\n\u{1F600}", true),
        ];
        for (input, active_invoke) in cases {
            let length = input.chars().count();
            for split in 0..=length {
                assert_eq!(mask_output(&char_split(input, split), active_invoke), input, "split {split} of {input:?}");
            }
        }
    }

}
