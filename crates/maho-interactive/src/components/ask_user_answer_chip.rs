//! Port of senpi `packages/coding-agent/src/modes/interactive/components/ask-user-answer-chip.ts`.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::LazyLock;

use maho_tui::tui::{
    Component, TuiMouseButton, TuiMouseEvent, TuiMouseEventResult, TuiMouseEventType,
};
use maho_tui::utils::{strip_terminal_sequences, truncate_to_width};
use regex::Regex;
use serde_json::Value;

use crate::theme::{Theme, ThemeColor};

pub const ASK_USER_QUESTION_ENTRY: &str = "ask-user:question";

static FRAME_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)^\[Answer to question ([^\]\r\n]+)\]\r?\n(.*)$").expect("valid frame regex"));
static NO_ANSWER_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:The user did not answer|The user dismissed|The pending question|This session has no user)")
        .expect("valid notice regex")
});
static WHITESPACE_RUN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").expect("valid whitespace regex"));

/// The frame type is owned by the builtin ask-user extension (plan todo 29); the chip only reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskUserAnswerFrame {
    pub request_id: String,
    pub body: String,
}

pub fn parse_ask_user_answer_frame(text: &str) -> Option<AskUserAnswerFrame> {
    let captures = FRAME_PATTERN.captures(text)?;
    Some(AskUserAnswerFrame {
        request_id: captures.get(1)?.as_str().to_string(),
        body: captures.get(2).map(|m| m.as_str().to_string()).unwrap_or_default(),
    })
}

pub fn get_ask_user_answer_headers(entries: &[Value], request_id: &str) -> Vec<String> {
    for entry in entries.iter().rev() {
        if entry.get("type").and_then(Value::as_str) != Some("custom") {
            continue;
        }
        if entry.get("customType").and_then(Value::as_str) != Some(ASK_USER_QUESTION_ENTRY) {
            continue;
        }
        let Some(data) = entry.get("data").filter(|data| data.is_object()) else {
            continue;
        };
        if data.get("requestId").and_then(Value::as_str) != Some(request_id) {
            continue;
        }
        let Some(headers) = data.get("headers").and_then(Value::as_array) else {
            continue;
        };
        return headers
            .iter()
            .filter_map(|header| header.as_str().map(str::to_owned))
            .collect();
    }
    Vec::new()
}

fn summary_rows(frame: &AskUserAnswerFrame, headers: &[String]) -> Vec<String> {
    let fallback_headers: Vec<String> = if headers.is_empty() {
        vec![frame.request_id.clone()]
    } else {
        headers.to_vec()
    };
    if NO_ANSWER_PATTERN.is_match(&frame.body) {
        return fallback_headers
            .iter()
            .map(|header| format!("↳ {header}: (no answer)"))
            .collect();
    }
    let mut answers: Vec<String> = Vec::new();
    let mut comment: Option<String> = None;
    let mut unanswered: Vec<String> = Vec::new();
    for line in frame.body.split('\n') {
        let Some(separator) = line.find(": ") else {
            continue;
        };
        if separator < 1 {
            continue;
        }
        let header = &line[..separator];
        let value = &line[separator + 2..];
        if header == "The user responded" {
            comment = Some(value.to_string());
        } else if header == "Unanswered" {
            unanswered = value.split(", ").map(str::to_owned).collect();
        } else {
            answers.push(format!("↳ {header}: {value}"));
        }
    }
    match comment {
        Some(comment) => {
            let key = headers
                .first()
                .or_else(|| unanswered.first())
                .cloned()
                .unwrap_or_else(|| frame.request_id.clone());
            answers.push(format!("↳ {key}: {}", Value::String(comment)));
        }
        None => {
            for header in &unanswered {
                answers.push(format!("↳ {header}: (no answer)"));
            }
        }
    }
    if answers.is_empty() {
        fallback_headers
            .iter()
            .map(|header| format!("↳ {header}: (no answer)"))
            .collect()
    } else {
        answers
    }
}

pub struct AskUserAnswerChip {
    rows: Vec<String>,
    full_body: Rc<RefCell<dyn Component>>,
    theme: Theme,
    expanded: bool,
}

impl AskUserAnswerChip {
    pub fn new(
        frame: &AskUserAnswerFrame,
        headers: &[String],
        full_body: Rc<RefCell<dyn Component>>,
        theme: Theme,
    ) -> Self {
        let rows = summary_rows(frame, headers)
            .into_iter()
            .map(|row| WHITESPACE_RUN.replace_all(&strip_terminal_sequences(&row), " ").into_owned())
            .collect();
        Self {
            rows,
            full_body,
            theme,
            expanded: false,
        }
    }

    pub fn expanded(&self) -> bool {
        self.expanded
    }
}

impl Component for AskUserAnswerChip {
    fn render(&mut self, width: usize) -> Vec<String> {
        if self.expanded {
            return self.full_body.borrow_mut().render(width);
        }
        self.rows
            .iter()
            .map(|row| {
                self.theme.fg(
                    ThemeColor::Muted,
                    &truncate_to_width(row, width, "…", false),
                )
            })
            .collect()
    }

    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        if event.button != TuiMouseButton::Left {
            return None;
        }
        if event.event_type == TuiMouseEventType::Press {
            return Some(TuiMouseEventResult {
                handled: true,
                render: Some(false),
                ..Default::default()
            });
        }
        if event.event_type != TuiMouseEventType::Click || event.click_count.unwrap_or(1) != 1 {
            return None;
        }
        self.expanded = !self.expanded;
        Some(TuiMouseEventResult {
            handled: true,
            ..Default::default()
        })
    }

    fn invalidate(&mut self) {
        self.full_body.borrow_mut().invalidate();
    }

    fn dispose(&mut self) {
        self.full_body.borrow_mut().dispose();
    }
}
