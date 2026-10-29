//! Approval question gates: a `tool_call` interceptor that marks approval-style questions sent to
//! the ask-user tools as `required`, so an unanswered approval keeps the gated action pending.
//!
//! Built clean-room from `docs/cleanroom/specs/question-gates.md` (see `PROVENANCE.md`).

use maho_ext_api::{EventKind, EventResult, Extension, ExtensionApi, ExtensionEvent};
use serde_json::Value;
use std::sync::Arc;

/// Extension id used when the extension is mounted.
pub const EXTENSION_ID: &str = "approval-gates";

/// The question tools whose calls are inspected (interface fact shared with the ask-user builtin).
pub const QUESTION_TOOL_NAMES: [&str; 2] = ["ask_user_question", "request_user_input"];

/// Headers that mark a question as an approval gate (interface fact; compared case-insensitively
/// after trimming). Each fits the 12-character header limit of the question tools.
pub const APPROVAL_HEADERS: [&str; 5] = ["Approval", "Authorize", "Authorise", "승인", "권한"];

/// Words that, as the first word of an option label, mark the question as an approval gate.
pub const APPROVAL_LABEL_WORDS: [&str; 3] = ["approve", "authorize", "authorise"];

/// Which detection rule forced the gate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GateRule {
    Header,
    Label,
}

impl GateRule {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Header => "header",
            Self::Label => "label",
        }
    }
}

/// What the interceptor records when it forces a gate. Carries only the tool name, the rule kind
/// and the matched question's trimmed header; never question text, labels or descriptions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GateDiagnostic {
    pub tool_name: String,
    pub rule: GateRule,
    pub header: Option<String>,
}

impl GateDiagnostic {
    /// One debug-level line for the diagnostic.
    pub fn line(&self) -> String {
        let mut fields = serde_json::Map::new();
        fields.insert("level".into(), Value::from("debug"));
        fields.insert("event".into(), Value::from("approval_gate_forced"));
        fields.insert("tool".into(), Value::from(self.tool_name.as_str()));
        fields.insert("rule".into(), Value::from(self.rule.as_str()));
        if let Some(header) = &self.header {
            fields.insert("header".into(), Value::from(header.as_str()));
        }
        Value::Object(fields).to_string()
    }
}

/// JavaScript `String.prototype.trim` whitespace: WhiteSpace (TAB, VT, FF, SP, NBSP, ZWNBSP and
/// the `Zs` category) plus LineTerminator (LF, CR, LS, PS). U+0085 is not included.
const fn is_js_trim_char(c: char) -> bool {
    matches!(
        c,
        '\u{0009}'
            | '\u{000A}'
            | '\u{000B}'
            | '\u{000C}'
            | '\u{000D}'
            | '\u{0020}'
            | '\u{00A0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200A}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202F}'
            | '\u{205F}'
            | '\u{3000}'
            | '\u{FEFF}'
    )
}

/// Trim with the JavaScript `String.prototype.trim` character set.
pub fn js_trim(text: &str) -> &str {
    text.trim_matches(is_js_trim_char)
}

/// Header rule: the trimmed header equals one of [`APPROVAL_HEADERS`], case-insensitively.
pub fn is_approval_header(header: &str) -> bool {
    let header = js_trim(header).to_lowercase();
    APPROVAL_HEADERS
        .iter()
        .any(|candidate| candidate.to_lowercase() == header)
}

/// Label rule: the trimmed label starts with one of [`APPROVAL_LABEL_WORDS`] (ASCII
/// case-insensitive) and the next character, if any, is not an ASCII letter.
pub fn is_approval_label(label: &str) -> bool {
    let label = js_trim(label).as_bytes();
    APPROVAL_LABEL_WORDS.iter().any(|word| {
        let word = word.as_bytes();
        label.len() >= word.len()
            && label[..word.len()].eq_ignore_ascii_case(word)
            && !label
                .get(word.len())
                .is_some_and(u8::is_ascii_alphabetic)
    })
}

fn trimmed_header(question: &serde_json::Map<String, Value>) -> Option<String> {
    question
        .get("header")
        .and_then(Value::as_str)
        .map(js_trim)
        .filter(|header| !header.is_empty())
        .map(str::to_owned)
}

/// Detect an approval gate in a question tool's input. Header rule over every question first,
/// then the label rule; the first hit wins. Malformed entries are skipped.
pub fn detect_gate(input: &Value) -> Option<(GateRule, Option<String>)> {
    let entries = input.as_object()?.get("questions")?.as_array()?;
    let questions = || entries.iter().filter_map(Value::as_object);
    for question in questions() {
        if question
            .get("header")
            .and_then(Value::as_str)
            .is_some_and(is_approval_header)
        {
            return Some((GateRule::Header, trimmed_header(question)));
        }
    }
    for question in questions() {
        let Some(options) = question.get("options").and_then(Value::as_array) else {
            continue;
        };
        let labelled = options
            .iter()
            .filter_map(Value::as_object)
            .filter_map(|option| option.get("label").and_then(Value::as_str))
            .any(is_approval_label);
        if labelled {
            return Some((GateRule::Label, trimmed_header(question)));
        }
    }
    None
}

/// The interceptor: when `tool_name` is a question tool, its input looks like an approval gate and
/// `required` is not already `true`, set `required: true` and return the diagnostic. Every other
/// input is left untouched and yields `None`.
pub fn force_required(tool_name: &str, input: &mut Value) -> Option<GateDiagnostic> {
    if !QUESTION_TOOL_NAMES.contains(&tool_name) {
        return None;
    }
    if input.get("required").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let (rule, header) = detect_gate(input)?;
    input
        .as_object_mut()?
        .insert("required".into(), Value::Bool(true));
    Some(GateDiagnostic {
        tool_name: tool_name.to_owned(),
        rule,
        header,
    })
}

fn debug_enabled() -> bool {
    maho_core::brand::env_value("APPROVAL_GATES_DEBUG", &maho_core::config::current_env())
        .as_deref()
        == Some("1")
}

/// The extension: stateless, one `tool_call` handler per session.
#[derive(Clone, Copy, Debug, Default)]
pub struct ApprovalGates;

impl Extension for ApprovalGates {
    fn register(&self, api: &mut ExtensionApi) {
        api.on(
            EventKind::ToolCall,
            Arc::new(|event, _ctx| {
                Box::pin(async move {
                    if let ExtensionEvent::ToolCall(call) = event
                        && let Some(diagnostic) = force_required(&call.tool_name, &mut call.input)
                        && debug_enabled()
                    {
                        maho_core::output_guard::maho_write_stderr(&format!(
                            "{}\n",
                            diagnostic.line()
                        ));
                    }
                    Ok(EventResult::None)
                })
            }),
        );
    }
}
