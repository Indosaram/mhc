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
}
