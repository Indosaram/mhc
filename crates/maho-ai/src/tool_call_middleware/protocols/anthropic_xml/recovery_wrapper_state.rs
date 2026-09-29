//! Port of senpi packages/ai/src/tool-call-middleware/protocols/anthropic-xml/recovery-wrapper-state.ts.

use super::invoke_stream_helpers::find_function_calls_close_tag;
use super::invoke_tag_scanner::find_invoke_open_tag;
use super::stream_boundary::ANTHROPIC_XML_MAX_RETAINED_FRAGMENT_LENGTH;
use crate::types::Tool;

pub enum RecoveryWrapperAction {
    Text { text: String },
    Known { text_before: String, opening: String, tool: Tool },
    Closed { text: String },
    Overflow { text: String, retained_length: usize, retains_wrapper: bool, next_character: Option<char> },
}

pub struct RecoveryWrapperState<R>
where
    R: Fn(&str) -> Option<Tool>,
{
    before_known: String,
    tag: String,
    recovered: bool,
    opening: String,
    resolve_tool: R,
}

impl<R> RecoveryWrapperState<R>
where
    R: Fn(&str) -> Option<Tool>,
{
    pub fn new(opening: impl Into<String>, resolve_tool: R) -> Self {
        Self { before_known: String::new(), tag: String::new(), recovered: false, opening: opening.into(), resolve_tool }
    }

    pub fn feed(&mut self, character: char) -> Vec<RecoveryWrapperAction> {
        if self.tag.is_empty() {
            if character == '<' {
                self.tag.push('<');
                return Vec::new();
            }
            return self.literal(&character.to_string());
        }
        if character == '<' {
            let tag = std::mem::take(&mut self.tag);
            self.tag.push('<');
            return self.literal(&tag);
        }
        if let Some(overflow) = self.pre_append_overflow(character) {
            return vec![overflow];
        }
        self.tag.push(character);
        if character != '>' {
            return self.check_overflow();
        }

        let tag = std::mem::take(&mut self.tag);
        let invoke = find_invoke_open_tag(&tag, 0);
        if let Some(invoke) = &invoke
            && invoke.index == 0
            && invoke.length == tag.len()
            && let Some(tool) = (self.resolve_tool)(&invoke.tool_name)
        {
            let text_before = std::mem::take(&mut self.before_known);
            self.recovered = true;
            return vec![RecoveryWrapperAction::Known { text_before, opening: tag, tool }];
        }
        let close = find_function_calls_close_tag(&tag, 0);
        if let Some(close) = &close
            && close.index == 0
            && close.length == tag.len()
        {
            let text = if self.recovered { String::new() } else { format!("{}{}{}", self.opening, self.before_known, tag) };
            return vec![RecoveryWrapperAction::Closed { text }];
        }
        self.literal(&tag)
    }

    pub fn finish(&self) -> String {
        if self.recovered { self.tag.clone() } else { format!("{}{}{}", self.opening, self.before_known, self.tag) }
    }

    fn literal(&mut self, text: &str) -> Vec<RecoveryWrapperAction> {
        if self.recovered {
            return if !text.is_empty() { vec![RecoveryWrapperAction::Text { text: text.to_string() }] } else { Vec::new() };
        }
        self.before_known.push_str(text);
        self.check_overflow()
    }

    fn check_overflow(&mut self) -> Vec<RecoveryWrapperAction> {
        let retained_length = self.retained_length();
        if retained_length != ANTHROPIC_XML_MAX_RETAINED_FRAGMENT_LENGTH {
            return Vec::new();
        }
        vec![self.flush_overflow(retained_length)]
    }

    fn pre_append_overflow(&mut self, character: char) -> Option<RecoveryWrapperAction> {
        let retained_length = self.retained_length();
        if retained_length + character.len_utf8() > ANTHROPIC_XML_MAX_RETAINED_FRAGMENT_LENGTH {
            let RecoveryWrapperAction::Overflow { text, retained_length, retains_wrapper, .. } = self.flush_overflow(retained_length) else {
                unreachable!()
            };
            Some(RecoveryWrapperAction::Overflow { text, retained_length, retains_wrapper, next_character: Some(character) })
        } else {
            None
        }
    }

    fn retained_length(&self) -> usize {
        if self.recovered { self.tag.len() } else { self.opening.len() + self.before_known.len() + self.tag.len() }
    }

    fn flush_overflow(&mut self, retained_length: usize) -> RecoveryWrapperAction {
        let text = if self.recovered { self.tag.clone() } else { format!("{}{}{}", self.opening, self.before_known, self.tag) };
        self.tag.clear();
        RecoveryWrapperAction::Overflow { text, retained_length, retains_wrapper: self.recovered, next_character: None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool(name: &str) -> Tool {
        Tool { name: name.into(), description: "d".into(), parameters: json!({"type": "object"}), freeform: None, constrained_sampling: None }
    }

    fn feed_str<R: Fn(&str) -> Option<Tool>>(state: &mut RecoveryWrapperState<R>, text: &str) -> Vec<RecoveryWrapperAction> {
        let mut events = Vec::new();
        for c in text.chars() {
            events.extend(state.feed(c));
        }
        events
    }

    #[test]
    fn recognizes_a_known_invoke_and_emits_known_action() {
        let tools = vec![tool("get_weather")];
        let resolver = move |name: &str| tools.iter().find(|t| t.name == name).cloned();
        let mut state = RecoveryWrapperState::new("<function_calls>", resolver);
        let events = feed_str(&mut state, "<invoke name=\"get_weather\">");
        assert!(matches!(events.last(), Some(RecoveryWrapperAction::Known { opening, .. }) if opening == "<invoke name=\"get_weather\">"));
    }

    #[test]
    fn unknown_open_tag_is_emitted_as_literal_text_before_known() {
        let tools: Vec<Tool> = Vec::new();
        let resolver = move |name: &str| tools.iter().find(|t| t.name == name).cloned();
        let mut state = RecoveryWrapperState::new("<function_calls>", resolver);
        let events = feed_str(&mut state, "<invoke name=\"unknown\">");
        assert!(events.is_empty());
        assert_eq!(state.finish(), "<function_calls><invoke name=\"unknown\">");
    }

    #[test]
    fn closing_tag_before_any_known_invoke_emits_closed_with_full_text() {
        let tools: Vec<Tool> = Vec::new();
        let resolver = move |name: &str| tools.iter().find(|t| t.name == name).cloned();
        let mut state = RecoveryWrapperState::new("<function_calls>", resolver);
        let events = feed_str(&mut state, "</function_calls>");
        assert!(matches!(events.last(), Some(RecoveryWrapperAction::Closed { text }) if text == "<function_calls></function_calls>"));
    }

    #[test]
    fn closing_tag_after_recovery_emits_closed_with_empty_text() {
        let tools = vec![tool("t")];
        let resolver = move |name: &str| tools.iter().find(|t| t.name == name).cloned();
        let mut state = RecoveryWrapperState::new("<function_calls>", resolver);
        feed_str(&mut state, "<invoke name=\"t\">");
        let events = feed_str(&mut state, "</function_calls>");
        assert!(matches!(events.last(), Some(RecoveryWrapperAction::Closed { text }) if text.is_empty()));
    }

    #[test]
    fn finish_returns_tag_only_after_recovery_else_full_prefix() {
        let tools: Vec<Tool> = Vec::new();
        let resolver = move |name: &str| tools.iter().find(|t| t.name == name).cloned();
        let mut state = RecoveryWrapperState::new("<function_calls>", resolver);
        feed_str(&mut state, "abc");
        assert_eq!(state.finish(), "<function_calls>abc");
    }
}
