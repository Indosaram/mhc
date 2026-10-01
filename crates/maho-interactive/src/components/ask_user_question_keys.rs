//! Port of senpi `packages/coding-agent/src/modes/interactive/components/ask-user-question-keys.ts`.
//!
//! Key dispatch for the ask-user question overlay. Split from the component to keep every sibling
//! under 250 LOC; handlers operate on the shared state and the component's inputs.

use maho_tui::keybindings::get_keybindings;
use maho_tui::keys::matches_key;
use maho_tui::tui::Component;

use super::ask_user_question::AskUserQuestionComponent;
use super::ask_user_question_state::{QuestionFocus, QuestionStatus};

fn is_printable(data: &str) -> bool {
    data.encode_utf16().count() == 1 && data >= " " && data != "\x7f"
}

pub fn handle_ask_user_key_input(component: &mut AskUserQuestionComponent, data: &str) {
    let kb = get_keybindings();
    if matches_key(data, "ctrl+c") {
        component.finish(QuestionStatus::Cancelled, None);
        return;
    }
    if matches_key(data, "ctrl+enter") {
        if component.state.focus == QuestionFocus::OwnAnswer {
            component.commit_own_answer();
        }
        component.attempt_submit();
        return;
    }
    match component.state.focus {
        QuestionFocus::OwnAnswer => handle_own_answer_key(component, data),
        QuestionFocus::Submit => handle_submit_key(component, data),
        QuestionFocus::Options => handle_options_key(component, data, &kb),
    }
}

fn save_typed_own_answer(component: &mut AskUserQuestionComponent) {
    if component.own_answer_input.borrow().get_value().trim() != "" {
        component.commit_own_answer();
    } else {
        component.own_answer_input.borrow_mut().set_value("");
    }
}

fn handle_own_answer_key(component: &mut AskUserQuestionComponent, data: &str) {
    let kb = get_keybindings();
    let row = component.state.own_answer_row_index();
    if kb.matches(data, "tui.select.confirm") || data == "\n" {
        component.commit_own_answer();
        component.state.advance();
        component.update_all();
        return;
    }
    if kb.matches(data, "tui.select.cancel") {
        component.own_answer_input.borrow_mut().set_value("");
        component.state.leave_own_answer(row as isize);
        component.update_all();
        return;
    }
    if kb.matches(data, "tui.select.up") {
        save_typed_own_answer(component);
        component.state.leave_own_answer(row as isize - 1);
        component.update_all();
        return;
    }
    if kb.matches(data, "tui.select.down") {
        save_typed_own_answer(component);
        component.state.leave_own_answer(row as isize);
        component.update_all();
        return;
    }
    if matches_key(data, "tab") || matches_key(data, "shift+tab") {
        save_typed_own_answer(component);
        component.state.switch_tab(if matches_key(data, "tab") { 1 } else { -1 });
        component.update_all();
        return;
    }
    if matches_key(data, "backspace") && component.own_answer_input.borrow().get_value() == "" {
        component.state.leave_own_answer(row as isize);
        component.update_all();
        return;
    }
    component.own_answer_input.borrow_mut().handle_input(data);
    component.emit_progress();
}

fn handle_submit_key(component: &mut AskUserQuestionComponent, data: &str) {
    let kb = get_keybindings();
    if kb.matches(data, "tui.input.submit") || data == "\n" {
        if component.state.is_comment_focused() {
            component.attempt_submit();
        } else {
            let row = component.state.submit_row_index;
            component.state.jump_to_question(row);
            component.update_all();
        }
        return;
    }
    if kb.matches(data, "tui.select.cancel") {
        if component.state.request.wait_for_answer {
            component.state.return_to_options();
        } else {
            component.finish(QuestionStatus::Cancelled, None);
        }
        component.update_all();
        return;
    }
    if kb.matches(data, "tui.select.up") || kb.matches(data, "tui.select.down") {
        component
            .state
            .move_submit_row(if kb.matches(data, "tui.select.up") { -1 } else { 1 });
        component.update_all();
        return;
    }
    if matches_key(data, "shift+tab") || matches_key(data, "tab") {
        component.state.switch_tab(if matches_key(data, "tab") { 1 } else { -1 });
        component.update_all();
        return;
    }
    let comment_has_text = component.comment_input.borrow().get_value() != "";
    if matches_key(data, "left") || matches_key(data, "right") {
        if !component.state.is_comment_focused() || !comment_has_text {
            component.state.switch_tab(if matches_key(data, "right") { 1 } else { -1 });
            component.update_all();
            return;
        }
    }
    if matches_key(data, "backspace") && component.state.is_comment_focused() && !comment_has_text {
        component.state.move_submit_row(-1);
        component.update_all();
        return;
    }
    if !component.state.is_comment_focused() {
        if !is_printable(data) {
            return;
        }
        component.state.focus_comment();
    }
    component.comment_input.borrow_mut().handle_input(data);
    component.state.comment = Some(component.comment_input.borrow().get_value().to_string());
    component.update_all();
    component.emit_progress();
}

fn handle_options_key(
    component: &mut AskUserQuestionComponent,
    data: &str,
    kb: &maho_tui::keybindings::KeybindingsManager,
) {
    if kb.matches(data, "tui.select.cancel") {
        if !component.state.request.wait_for_answer || component.state.request_dismiss() {
            component.finish(QuestionStatus::Cancelled, None);
        } else {
            component.update_all();
        }
        return;
    }
    if matches_key(data, "tab") || matches_key(data, "right") {
        component.state.switch_tab(1);
        component.update_all();
        return;
    }
    if matches_key(data, "shift+tab") || matches_key(data, "left") {
        component.state.switch_tab(-1);
        component.update_all();
        return;
    }
    if kb.matches(data, "tui.select.up") || data == "k" {
        component.state.highlight_index = component.state.highlight_index.saturating_sub(1);
        component.update_all();
        return;
    }
    if kb.matches(data, "tui.select.down") || data == "j" {
        component.state.highlight_index = component
            .state
            .highlight_index
            .saturating_add(1)
            .min(component.state.own_answer_row_index());
        component.update_all();
        return;
    }
    if matches_key(data, "backspace") {
        let id = component.state.active_question().id.clone();
        component.state.clear_answer(&id);
        component.emit_progress();
        component.update_all();
        return;
    }
    if data.len() == 1 && data >= "1" && data <= "9" {
        let index = usize::from(data.as_bytes()[0] - b'1');
        let question = component.state.active_question().clone();
        if let Some(option) = question.options.get(index) {
            component.state.activate_option(&question.id, &option.label);
            component.emit_progress();
            if !question.multi_select {
                if component.state.request.questions.len() == 1 {
                    component.attempt_submit();
                } else {
                    component.state.advance();
                }
            }
            component.update_all();
        }
        return;
    }
    if matches_key(data, "space") {
        activate_highlighted(component, false);
        return;
    }
    if kb.matches(data, "tui.select.confirm") || data == "\n" {
        activate_highlighted(component, true);
        return;
    }
    if data == "c" {
        component.state.enter_submit();
        component.update_all();
        return;
    }
    if is_printable(data) {
        component.open_own_answer(Some(data));
        component.update_all();
    }
}

fn activate_highlighted(component: &mut AskUserQuestionComponent, confirm: bool) {
    let index = component.state.highlight_index;
    if index == component.state.own_answer_row_index() {
        component.open_own_answer(None);
        return;
    }
    let question = component.state.active_question().clone();
    let Some(option) = question.options.get(index) else {
        return;
    };
    component.state.activate_option(&question.id, &option.label);
    component.emit_progress();
    component.update_all();
    if confirm {
        if !question.multi_select && component.state.request.questions.len() == 1 {
            component.attempt_submit();
        } else if !question.multi_select {
            component.state.advance();
            component.update_all();
        }
    }
}
