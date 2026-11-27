//! Extension UI request kinds and answer text shaping (`answer-shape.ts`).
//!
//! Maps a relayed question's declared kind (`question`, `select`, `confirm`, `input`, `editor`, or
//! `None`) and answer text onto the `extension_ui_response` payload fields that kind expects.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const UI_REQUEST_KINDS: [&str; 5] = ["question", "select", "confirm", "input", "editor"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiRequestKind {
    Question,
    Select,
    Confirm,
    Input,
    Editor,
}

impl UiRequestKind {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "question" => Some(Self::Question),
            "select" => Some(Self::Select),
            "confirm" => Some(Self::Confirm),
            "input" => Some(Self::Input),
            "editor" => Some(Self::Editor),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Question => "question",
            Self::Select => "select",
            Self::Confirm => "confirm",
            Self::Input => "input",
            Self::Editor => "editor",
        }
    }
}

pub fn is_ui_request_kind(value: &str) -> bool {
    UiRequestKind::parse(value).is_some()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnswerShape {
    Ok(Value),
    Refused(String),
}

fn parse_confirm_answer(text: &str) -> Option<bool> {
    match text.trim().to_ascii_lowercase().as_str() {
        "yes" | "y" | "true" => Some(true),
        "no" | "n" | "false" => Some(false),
        _ => None,
    }
}

/// Returns true if `text` contains only whitespace and Unicode format characters (`\p{Cf}`).
pub fn is_blank_answer(text: &str) -> bool {
    text.chars()
        .all(|ch| ch.is_whitespace() || is_format_char(ch))
}

fn is_format_char(ch: char) -> bool {
    matches!(
        ch,
        '\u{00AD}'
            | '\u{0600}'..='\u{0605}'
            | '\u{061C}'
            | '\u{06DD}'
            | '\u{070F}'
            | '\u{0890}'..='\u{0891}'
            | '\u{08E2}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206F}'
            | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}'
    )
}

fn undeclared_fields(text: &str) -> Value {
    match parse_confirm_answer(text) {
        Some(confirmed) => json!({
            "value": text,
            "answers": {},
            "comment": text,
            "confirmed": confirmed,
        }),
        None => json!({
            "value": text,
            "answers": {},
            "comment": text,
        }),
    }
}

/// Converts a relayed question kind + raw answer text into `extension_ui_response` fields.
pub fn answer_shape(kind: Option<UiRequestKind>, text: &str) -> AnswerShape {
    match kind {
        None => {
            if is_blank_answer(text) {
                AnswerShape::Refused(
                    "The question declared no request kind, so the answer needs text; this one is blank. An input or editor that takes an empty answer must be reported with its request kind.".to_owned(),
                )
            } else {
                AnswerShape::Ok(undeclared_fields(text))
            }
        }
        Some(UiRequestKind::Input | UiRequestKind::Editor) => {
            AnswerShape::Ok(json!({ "value": text }))
        }
        Some(UiRequestKind::Select) => {
            if is_blank_answer(text) {
                AnswerShape::Refused(
                    "A select answer names one of its options; the text is blank.".to_owned(),
                )
            } else {
                AnswerShape::Ok(json!({ "value": text }))
            }
        }
        Some(UiRequestKind::Question) => {
            if is_blank_answer(text) {
                AnswerShape::Refused(
                    "A question answer needs text; this one is blank.".to_owned(),
                )
            } else {
                AnswerShape::Ok(json!({ "answers": {}, "comment": text }))
            }
        }
        Some(UiRequestKind::Confirm) => match parse_confirm_answer(text) {
            Some(confirmed) => AnswerShape::Ok(json!({ "confirmed": confirmed })),
            None => AnswerShape::Refused(
                "A confirm answer is yes or no (also y/n, true/false; any case, surrounding spaces ignored).".to_owned(),
            ),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shapes_declared_and_undeclared_answers() {
        assert_eq!(
            answer_shape(None, "yes"),
            AnswerShape::Ok(json!({
                "value": "yes",
                "answers": {},
                "comment": "yes",
                "confirmed": true
            }))
        );
        assert_eq!(
            answer_shape(None, "\u{200B}   \u{FEFF}"),
            AnswerShape::Refused(
                "The question declared no request kind, so the answer needs text; this one is blank. An input or editor that takes an empty answer must be reported with its request kind.".to_owned()
            )
        );
        assert_eq!(
            answer_shape(Some(UiRequestKind::Input), ""),
            AnswerShape::Ok(json!({ "value": "" }))
        );
        assert_eq!(
            answer_shape(Some(UiRequestKind::Confirm), "  No "),
            AnswerShape::Ok(json!({ "confirmed": false }))
        );
        assert!(matches!(
            answer_shape(Some(UiRequestKind::Confirm), "maybe"),
            AnswerShape::Refused(_)
        ));
    }
}
