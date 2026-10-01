//! Port of senpi `packages/coding-agent/src/modes/interactive/components/ask-user-async-widget.ts`.

use maho_tui::components::text::Text;
use maho_tui::terminal_text::sanitize_terminal_label;
use maho_tui::tui::{
    Component, TuiMouseButton, TuiMouseEvent, TuiMouseEventResult, TuiMouseEventType,
};
use maho_tui::utils::{truncate_to_width, visible_width};

use crate::theme::{Theme, ThemeColor};

use super::ask_user_answer_key::{ask_user_answer_key_hint, key_text};
use super::ask_user_countdown::AskUserCountdown;
use super::ask_user_question_state::{
    format_countdown_label, QuestionAnswer, QuestionDraft, QuestionRequest, QuestionResponse,
    QuestionStatus,
};

pub const ASK_USER_WIDGET_KEY: &str = "ask-user";

const INDENT: &str = "  ";

fn has_answer(answer: Option<&QuestionAnswer>) -> bool {
    let Some(answer) = answer else {
        return false;
    };
    if !answer.selected.is_empty() {
        return true;
    }
    answer.text.as_ref().is_some_and(|text| !text.trim().is_empty())
}

pub fn unanswered_ids(request: &QuestionRequest, draft: &QuestionDraft) -> Vec<String> {
    request
        .questions
        .iter()
        .filter(|question| !has_answer(draft.answers.get(&question.id)))
        .map(|question| question.id.clone())
        .collect()
}

pub fn build_comment_response(
    request: &QuestionRequest,
    draft: &QuestionDraft,
    comment: &str,
) -> QuestionResponse {
    QuestionResponse {
        status: QuestionStatus::CommentSubmitted,
        answers: draft.answers.clone(),
        comment: Some(comment.to_string()),
        unanswered: unanswered_ids(request, draft),
        auto_resolved_after_ms: None,
    }
}

pub fn build_timed_out_response(
    request: &QuestionRequest,
    draft: &QuestionDraft,
    auto_resolved_after_ms: u64,
) -> QuestionResponse {
    let comment = draft
        .comment
        .as_ref()
        .map(|comment| comment.trim().to_string())
        .filter(|comment| !comment.is_empty());
    QuestionResponse {
        status: QuestionStatus::TimedOut,
        answers: draft.answers.clone(),
        comment,
        unanswered: unanswered_ids(request, draft),
        auto_resolved_after_ms: Some(auto_resolved_after_ms),
    }
}

pub fn render_status_line(
    theme: &Theme,
    unanswered: usize,
    countdown_label: &str,
    pending_count: usize,
) -> String {
    let countdown = if countdown_label.is_empty() {
        String::new()
    } else {
        theme.fg(ThemeColor::Muted, &format!(" · {countdown_label}"))
    };
    let label = if pending_count > 1 {
        format!(" {pending_count} questions pending")
    } else {
        format!(" Question pending ({unanswered} unanswered)")
    };
    format!(
        "{}{}{}",
        theme.fg(ThemeColor::Accent, &theme.bold("?")),
        theme.fg(ThemeColor::Text, &label),
        countdown
    )
}

pub fn render_question_line(
    theme: &Theme,
    question: &super::ask_user_question_state::Question,
) -> String {
    format!(
        "{INDENT}{}{}{}",
        theme.fg(ThemeColor::Text, &theme.bold(&question.header)),
        theme.fg(ThemeColor::Muted, " — "),
        theme.fg(ThemeColor::Text, &question.question)
    )
}

pub fn render_options_line(
    theme: &Theme,
    question: &super::ask_user_question_state::Question,
    remaining: usize,
) -> String {
    let mut parts: Vec<String> = question
        .options
        .iter()
        .enumerate()
        .map(|(index, option)| {
            format!(
                "{}{}",
                theme.fg(ThemeColor::Dim, &format!("{}", index + 1)),
                theme.fg(ThemeColor::Muted, &format!(" {}", option.label))
            )
        })
        .collect();
    parts.push(theme.fg(ThemeColor::Muted, "own answer"));
    if remaining > 0 {
        parts.push(theme.fg(
            ThemeColor::Muted,
            &format!("+{remaining} more question{}", if remaining == 1 { "" } else { "s" }),
        ));
    }
    format!("{INDENT}{}", parts.join(&theme.fg(ThemeColor::Muted, " · ")))
}

pub fn render_answer_hint(theme: &Theme, env: &std::collections::HashMap<String, String>) -> String {
    let shortcut = ask_user_answer_key_hint(env);
    let keys = if shortcut.is_empty() {
        "enter".to_string()
    } else {
        format!("enter or {shortcut}")
    };
    [
        format!("{}{}", theme.fg(ThemeColor::Dim, &keys), theme.fg(ThemeColor::Muted, " to answer")),
        theme.fg(ThemeColor::Dim, "/answer"),
        theme.fg(ThemeColor::Muted, "or just type your reply"),
    ]
    .join(&theme.fg(ThemeColor::Muted, " · "))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WidgetAction {
    Option(usize),
    OwnAnswer,
    Expand,
    Next,
}

struct WidgetHit {
    action: WidgetAction,
    row: usize,
    start_column: usize,
    end_column: usize,
}

pub struct AskUserAsyncWidgetOptions {
    pub request: QuestionRequest,
    pub draft: QuestionDraft,
    pub timeout_ms: u64,
    pub now_ms: u64,
    pub get_deadline_at_ms: Option<Box<dyn Fn() -> u64>>,
    pub pending_count: usize,
    pub theme: Theme,
    pub env: std::collections::HashMap<String, String>,
    pub on_expire: Box<dyn FnMut()>,
    pub mouse_capture_active: bool,
    pub on_option_click: Option<Box<dyn FnMut(usize)>>,
    pub on_own_answer_click: Option<Box<dyn FnMut()>>,
    pub on_expand_click: Option<Box<dyn FnMut()>>,
    pub on_next_question: Option<Box<dyn FnMut()>>,
}

pub struct AskUserAsyncWidget {
    request: QuestionRequest,
    draft: QuestionDraft,
    countdown: Option<AskUserCountdown>,
    countdown_label: String,
    pending_count: usize,
    theme: Theme,
    env: std::collections::HashMap<String, String>,
    get_deadline_at_ms: Option<Box<dyn Fn() -> u64>>,
    on_expire: Box<dyn FnMut()>,
    mouse_capture_active: bool,
    on_option_click: Option<Box<dyn FnMut(usize)>>,
    on_own_answer_click: Option<Box<dyn FnMut()>>,
    on_expand_click: Option<Box<dyn FnMut()>>,
    on_next_question: Option<Box<dyn FnMut()>>,
    hits: Vec<WidgetHit>,
    rendered_width: usize,
}

impl AskUserAsyncWidget {
    pub fn new(options: AskUserAsyncWidgetOptions) -> Self {
        let mut widget = Self {
            request: options.request,
            draft: options.draft,
            countdown: None,
            countdown_label: String::new(),
            pending_count: options.pending_count,
            theme: options.theme,
            env: options.env,
            get_deadline_at_ms: options.get_deadline_at_ms,
            on_expire: options.on_expire,
            mouse_capture_active: options.mouse_capture_active,
            on_option_click: options.on_option_click,
            on_own_answer_click: options.on_own_answer_click,
            on_expand_click: options.on_expand_click,
            on_next_question: options.on_next_question,
            hits: Vec::new(),
            rendered_width: 0,
        };
        if options.timeout_ms > 0 || widget.get_deadline_at_ms.is_some() {
            let external = widget.get_deadline_at_ms.as_ref().map(|f| f());
            widget.countdown = Some(AskUserCountdown::new(options.timeout_ms, options.now_ms, external));
        }
        widget.tick(options.now_ms);
        widget
    }

    pub fn tick(&mut self, now_ms: u64) {
        let Some(countdown) = &mut self.countdown else {
            return;
        };
        let external = self.get_deadline_at_ms.as_ref().map(|f| f());
        let Some((remaining, expired)) = countdown.tick(now_ms, external) else {
            return;
        };
        self.countdown_label = format_countdown_label(remaining as f64);
        if expired {
            (self.on_expire)();
        }
    }

    fn activate(&mut self, action: WidgetAction) {
        match action {
            WidgetAction::Option(index) => {
                if let Some(callback) = &mut self.on_option_click {
                    callback(index);
                }
            }
            WidgetAction::OwnAnswer => {
                if let Some(callback) = &mut self.on_own_answer_click {
                    callback();
                }
            }
            WidgetAction::Expand => {
                if let Some(callback) = &mut self.on_expand_click {
                    callback();
                }
            }
            WidgetAction::Next => {
                if let Some(callback) = &mut self.on_next_question {
                    callback();
                }
            }
        }
    }
}

fn push_action(
    hits: &mut Vec<WidgetHit>,
    lines: &mut Vec<String>,
    width: usize,
    text: &str,
    action: WidgetAction,
) {
    let line = truncate_to_width(text, width, "", false);
    hits.push(WidgetHit {
        action,
        row: lines.len(),
        start_column: 0,
        end_column: visible_width(&line),
    });
    lines.push(line);
}

impl Component for AskUserAsyncWidget {
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut hits: Vec<WidgetHit> = Vec::new();
        self.rendered_width = width;
        if width < 5 {
            self.hits = hits;
            return vec![truncate_to_width("Use keys to answer", width, "", false)];
        }
        let theme = self.theme.clone();
        let pending = unanswered_ids(&self.request, &self.draft);
        let shown = self
            .request
            .questions
            .iter()
            .find(|question| pending.first().is_some_and(|id| *id == question.id))
            .cloned();
        let mut lines: Vec<String> = Vec::new();
        push_action(
            &mut hits,
            &mut lines,
            width,
            &render_status_line(&theme, pending.len(), &self.countdown_label, self.pending_count),
            WidgetAction::Expand,
        );
        if let Some(shown) = shown {
            let sanitized = super::ask_user_question_state::Question {
                id: shown.id.clone(),
                header: sanitize_terminal_label(&shown.header),
                question: sanitize_terminal_label(&shown.question),
                options: shown.options.clone(),
                multi_select: shown.multi_select,
            };
            push_action(
                &mut hits,
                &mut lines,
                width,
                &render_question_line(&theme, &sanitized),
                WidgetAction::Expand,
            );
            let mut labels: Vec<String> = sanitized
                .options
                .iter()
                .map(|option| sanitize_terminal_label(&option.label))
                .collect();
            labels.push("own answer…".to_string());
            let mut line = String::new();
            for (index, label) in labels.iter().enumerate() {
                let button = format!("[ {} ]", truncate_to_width(label, width.saturating_sub(4), "…", false));
                if !line.is_empty() && visible_width(&line) + 2 + visible_width(&button) > width {
                    lines.push(line);
                    line = String::new();
                }
                if !line.is_empty() {
                    line.push_str("  ");
                }
                let start_column = visible_width(&line);
                line.push_str(&theme.fg(ThemeColor::Muted, &button));
                hits.push(WidgetHit {
                    action: if index == sanitized.options.len() {
                        WidgetAction::OwnAnswer
                    } else {
                        WidgetAction::Option(index)
                    },
                    row: lines.len(),
                    start_column,
                    end_column: visible_width(&line),
                });
            }
            if !line.is_empty() {
                lines.push(line);
            }
            if pending.len() > 1 {
                lines.push(truncate_to_width(
                    &theme.fg(
                        ThemeColor::Muted,
                        &format!(
                            "+{} more question{}",
                            pending.len() - 1,
                            if pending.len() == 2 { "" } else { "s" }
                        ),
                    ),
                    width,
                    "",
                    false,
                ));
            }
        }
        if self.pending_count > 1 {
            push_action(
                &mut hits,
                &mut lines,
                width,
                &theme.fg(
                    ThemeColor::Muted,
                    &format!(
                        "▸ +{} more · {} next question",
                        self.pending_count - 1,
                        key_text("app.question.next")
                    ),
                ),
                WidgetAction::Next,
            );
        }
        if self.mouse_capture_active {
            let term_program = self.env.get("TERM_PROGRAM").map(String::as_str).unwrap_or("");
            let bypass = if ["iTerm.app", "Apple_Terminal"].contains(&term_program) {
                "option+drag"
            } else {
                "shift+drag"
            };
            lines.extend(
                Text::with_padding(
                    theme.fg(ThemeColor::Muted, &format!("click an option · {bypass} to select text")),
                    0,
                    0,
                )
                .render(width),
            );
        }
        lines.extend(
            Text::with_padding(render_answer_hint(&theme, &self.env), 0, 0).render(width),
        );
        self.hits = hits;
        lines
    }

    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        if event.width != self.rendered_width
            || event.button != TuiMouseButton::Left
            || event.shift
            || event.alt
            || event.ctrl
            || (event.event_type != TuiMouseEventType::Press
                && (event.event_type != TuiMouseEventType::Click || event.click_count != Some(1)))
        {
            return None;
        }
        let hit = self.hits.iter().find(|hit| {
            hit.row as i64 == event.y
                && event.x >= hit.start_column as i64
                && event.x < hit.end_column as i64
        });
        let action = hit.map(|hit| hit.action)?;
        if event.event_type == TuiMouseEventType::Click {
            self.activate(action);
        }
        Some(TuiMouseEventResult {
            handled: true,
            ..Default::default()
        })
    }

    fn invalidate(&mut self) {}

    fn dispose(&mut self) {
        if let Some(countdown) = &mut self.countdown {
            countdown.dispose();
        }
    }
}
