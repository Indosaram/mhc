//! Port of senpi packages/ai/src/tool-call-middleware/protocols/anthropic-xml/stream-boundary.ts.

// Incomplete protocol fragments are rejected before retained input can exceed this many bytes.
pub const ANTHROPIC_XML_MAX_RETAINED_FRAGMENT_LENGTH: usize = 64 * 1024;

pub trait StreamBoundaryMatcher: Send {
    fn feed(&mut self, text: &str) -> bool;
}

pub enum PendingFragmentKind {
    Invoke,
    FunctionCalls,
    OpenTag,
}

pub struct PendingFragment {
    pub kind: PendingFragmentKind,
    pub matcher: Box<dyn StreamBoundaryMatcher>,
}

fn is_invoke_open_tag(tag: &str) -> bool {
    tag_matches(tag, "invoke", false)
}

fn is_invoke_close_tag(tag: &str) -> bool {
    tag_matches(tag, "invoke", true)
}

fn is_parameter_open_tag(tag: &str) -> bool {
    tag_matches(tag, "parameter", false)
}

fn is_parameter_close_tag(tag: &str) -> bool {
    tag_matches(tag, "parameter", true)
}

fn tag_matches(tag: &str, name: &str, closing: bool) -> bool {
    let Some(inner) = tag.strip_prefix('<') else { return false };
    let Some(inner) = inner.strip_suffix('>') else { return false };
    let inner = inner.trim_start();
    let inner = if closing {
        let Some(rest) = inner.strip_prefix('/') else { return false };
        rest.trim_start()
    } else {
        inner
    };
    let inner = inner.strip_prefix("antml:").unwrap_or(inner);
    if closing {
        inner.trim_end() == name
    } else if let Some(rest) = inner.strip_prefix(name) {
        rest.is_empty() || rest.starts_with(char::is_whitespace)
    } else {
        false
    }
}

struct InvokeBoundaryMatcher {
    invoke_depth: i64,
    parameter_invoke_depths: Vec<i64>,
    tag_chars: Option<String>,
}

impl InvokeBoundaryMatcher {
    fn new() -> Self {
        Self { invoke_depth: 0, parameter_invoke_depths: Vec::new(), tag_chars: None }
    }
}

impl StreamBoundaryMatcher for InvokeBoundaryMatcher {
    fn feed(&mut self, text: &str) -> bool {
        for character in text.chars() {
            match &mut self.tag_chars {
                None => {
                    if character == '<' {
                        self.tag_chars = Some(character.to_string());
                    }
                    continue;
                }
                Some(buf) => {
                    if character == '<' {
                        self.tag_chars = Some(character.to_string());
                        continue;
                    }
                    buf.push(character);
                    if character != '>' {
                        continue;
                    }
                }
            }

            let tag = self.tag_chars.take().unwrap();
            if is_invoke_open_tag(&tag) {
                self.invoke_depth += 1;
                continue;
            }
            if is_parameter_open_tag(&tag) {
                self.parameter_invoke_depths.push(self.invoke_depth);
                continue;
            }
            if is_parameter_close_tag(&tag) {
                if let Some(parameter_invoke_depth) = self.parameter_invoke_depths.pop()
                    && self.invoke_depth > parameter_invoke_depth
                {
                    self.invoke_depth = parameter_invoke_depth;
                }
                continue;
            }
            if !is_invoke_close_tag(&tag) || self.invoke_depth == 0 {
                continue;
            }

            self.invoke_depth -= 1;
            while let Some(&parameter_invoke_depth) = self.parameter_invoke_depths.last() {
                if parameter_invoke_depth <= self.invoke_depth {
                    break;
                }
                self.parameter_invoke_depths.pop();
            }
            if self.invoke_depth == 0 {
                return true;
            }
        }
        false
    }
}

struct ClosingTagMatcher {
    tag_name: &'static str,
    tag_chars: Option<String>,
}

impl StreamBoundaryMatcher for ClosingTagMatcher {
    fn feed(&mut self, text: &str) -> bool {
        let mut matched = false;
        for character in text.chars() {
            match &mut self.tag_chars {
                None => {
                    if character == '<' {
                        self.tag_chars = Some(character.to_string());
                    }
                    continue;
                }
                Some(buf) => {
                    if character == '<' {
                        self.tag_chars = Some(character.to_string());
                        continue;
                    }
                    buf.push(character);
                    if character == '>' {
                        let tag = self.tag_chars.take().unwrap();
                        matched = matched || tag_matches(&tag, self.tag_name, true);
                    }
                }
            }
        }
        matched
    }
}

pub fn create_closing_tag_matcher(tag_name: &'static str) -> Box<dyn StreamBoundaryMatcher> {
    Box::new(ClosingTagMatcher { tag_name, tag_chars: None })
}

struct TagEndMatcher;

impl StreamBoundaryMatcher for TagEndMatcher {
    fn feed(&mut self, text: &str) -> bool {
        text.contains('>')
    }
}

pub fn create_pending_fragment(kind: PendingFragmentKind, text: &str) -> PendingFragment {
    let mut matcher: Box<dyn StreamBoundaryMatcher> = match kind {
        PendingFragmentKind::Invoke => Box::new(InvokeBoundaryMatcher::new()),
        PendingFragmentKind::FunctionCalls => create_closing_tag_matcher("function_calls"),
        PendingFragmentKind::OpenTag => Box::new(TagEndMatcher),
    };
    matcher.feed(text);
    PendingFragment { kind, matcher }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invoke_boundary_matcher_signals_true_only_at_matching_close() {
        let mut fragment = create_pending_fragment(PendingFragmentKind::Invoke, "<invoke name=\"t\">");
        assert!(!fragment.matcher.feed("<parameter name=\"a\">1</parameter>"));
        assert!(fragment.matcher.feed("</invoke>"));
    }

    #[test]
    fn invoke_boundary_matcher_handles_nested_invoke_inside_parameter() {
        let mut fragment = create_pending_fragment(PendingFragmentKind::Invoke, "<invoke name=\"t\">");
        assert!(!fragment.matcher.feed("<parameter name=\"a\"><invoke name=\"nested\">x</invoke></parameter>"));
        assert!(fragment.matcher.feed("</invoke>"));
    }

    #[test]
    fn closing_tag_matcher_matches_function_calls_close() {
        let mut fragment = create_pending_fragment(PendingFragmentKind::FunctionCalls, "<function_calls>");
        assert!(fragment.matcher.feed("</function_calls>"));
    }

    #[test]
    fn tag_end_matcher_signals_on_any_close_bracket() {
        let mut fragment = create_pending_fragment(PendingFragmentKind::OpenTag, "<inv");
        assert!(fragment.matcher.feed("oke>"));
    }
}
