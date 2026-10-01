//! Port of senpi `packages/coding-agent/src/modes/interactive/components/ask-user-question.ts`.
//!
//! Ask-user question overlay: tab bar of question headers, numbered options with descriptions,
//! per-question own-answer editor, a Submit tab with review rows and the comment editor, submit
//! footer and a countdown chip. Layout lives in `ask_user_question_render.rs`, interaction rules
//! in `ask_user_question_state.rs` and key dispatch in `ask_user_question_keys.rs`.
//!
//! Two seams differ from the TypeScript source and are recorded in `parity.d/34.md`: senpi drives
//! the countdown with `setInterval` and the host clock, so this port exposes an explicit
//! [`AskUserQuestionComponent::tick`] (the crate's timer convention from todo 31); and the
//! `DynamicBorder` it wraps the overlay in lives in `components/dynamic-border.ts`, owned by plan
//! todo 32, so a private [`AskUserBorder`] renders the identical line until that module lands.

use std::cell::RefCell;
use std::rc::Rc;

use maho_tui::components::input::{Input, InputOptions};
use maho_tui::components::spacer::Spacer;
use maho_tui::components::text::Text;
use maho_tui::tui::{
    Component, Container, Focusable, TuiMouseEvent, TuiMouseEventResult, TuiMouseEventType,
};

use crate::theme::{Theme, ThemeColor};

use super::ask_user_countdown::AskUserCountdown;
use super::ask_user_question_mouse::{is_question_mouse_action, AskUserQuestionTabs};
use super::ask_user_question_render::{
    render_comment_label, render_hints_line, render_notice, render_own_answer_label,
    render_question_line, render_question_list, render_submit_line, render_submit_summary,
    render_tab_labels, render_title,
};
use super::ask_user_question_state::{
    format_countdown_label, AskUserQuestionState, QuestionAnswer, QuestionDraft, QuestionFocus,
    QuestionRequest, QuestionResponse, QuestionStatus, NOT_ANSWERED_NOTICE,
};
use maho_tui::components::mouse_region::MouseRegion;

struct AskUserBorder {
    theme: Theme,
}

impl Component for AskUserBorder {
    fn render(&mut self, width: usize) -> Vec<String> {
        vec![self.theme.fg(ThemeColor::Border, &"─".repeat(width.max(1)))]
    }
}

/// Mouse region handlers cannot borrow the component, so a click queues the call the TypeScript
/// closure would have made and `handle_mouse` drains the queue after the child dispatch returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AskUserAction {
    ClickOption(usize),
    OpenOwnAnswer,
    ClickSubmit,
}

pub struct AskUserQuestionOptions {
    pub theme: Theme,
    pub now_ms: u64,
    pub timeout_ms: Option<u64>,
    pub get_deadline_at_ms: Option<Box<dyn Fn() -> u64>>,
    pub on_progress: Option<ProgressCallback>,
    pub initial_draft: Option<QuestionDraft>,
    pub initial_question_index: Option<usize>,
}

impl AskUserQuestionOptions {
    pub fn new(theme: Theme) -> Self {
        Self {
            theme,
            now_ms: 0,
            timeout_ms: None,
            get_deadline_at_ms: None,
            on_progress: None,
            initial_draft: None,
            initial_question_index: None,
        }
    }
}

/// Draft notification on every selection or keystroke (drives the idle timer).
pub type ProgressCallback = Box<dyn FnMut(&QuestionDraft)>;

pub struct AskUserQuestionComponent {
    pub state: AskUserQuestionState,
    done: Box<dyn FnMut(QuestionResponse)>,
    theme: Theme,
    pub own_answer_input: Rc<RefCell<Input>>,
    pub comment_input: Rc<RefCell<Input>>,
    countdown: Option<AskUserCountdown>,
    timeout_ms: u64,
    get_deadline_at_ms: Option<Box<dyn Fn() -> u64>>,
    on_progress: Option<ProgressCallback>,
    root: Container,
    title_text: Rc<RefCell<Text>>,
    tab_text: Rc<RefCell<AskUserQuestionTabs>>,
    question_text: Rc<RefCell<Text>>,
    list_container: Rc<RefCell<Container>>,
    own_answer_container: Rc<RefCell<Container>>,
    submit_container: Rc<RefCell<Container>>,
    notice_text: Rc<RefCell<Text>>,
    submit_text: Rc<RefCell<Text>>,
    hints_text: Rc<RefCell<Text>>,
    pending: Rc<RefCell<Vec<AskUserAction>>>,
    countdown_label: String,
    settled: bool,
    focused: bool,
}

fn text_child(text: Text) -> Rc<RefCell<dyn Component>> {
    Rc::new(RefCell::new(text))
}

fn component_child<C: Component + 'static>(component: C) -> Rc<RefCell<dyn Component>> {
    Rc::new(RefCell::new(component))
}

fn container_child(container: Container) -> (Rc<RefCell<Container>>, Rc<RefCell<dyn Component>>) {
    let handle = Rc::new(RefCell::new(container));
    let child: Rc<RefCell<dyn Component>> = Rc::clone(&handle) as Rc<RefCell<dyn Component>>;
    (handle, child)
}

impl AskUserQuestionComponent {
    pub fn new(
        request: QuestionRequest,
        done: Box<dyn FnMut(QuestionResponse)>,
        mut options: AskUserQuestionOptions,
    ) -> Self {
        let timeout_ms = options.timeout_ms.unwrap_or(request.timeout_ms);
        let state = AskUserQuestionState::new(request);
        let own_answer_input = Rc::new(RefCell::new(Input::new(InputOptions::default())));
        let comment_input = Rc::new(RefCell::new(Input::new(InputOptions::default())));
        let title_text = Rc::new(RefCell::new(Text::with_padding("", 1, 0)));
        let tab_text = Rc::new(RefCell::new(AskUserQuestionTabs::default()));
        let question_text = Rc::new(RefCell::new(Text::with_padding("", 1, 0)));
        let notice_text = Rc::new(RefCell::new(Text::with_padding("", 1, 0)));
        let submit_text = Rc::new(RefCell::new(Text::with_padding("", 1, 0)));
        let hints_text = Rc::new(RefCell::new(Text::with_padding("", 1, 0)));
        let pending: Rc<RefCell<Vec<AskUserAction>>> = Rc::new(RefCell::new(Vec::new()));

        let mut root = Container::new();
        root.add_child(component_child(AskUserBorder { theme: options.theme.clone() }));
        root.add_child(text_child_spacer(1));
        root.add_child(Rc::clone(&title_text) as Rc<RefCell<dyn Component>>);
        root.add_child(Rc::clone(&tab_text) as Rc<RefCell<dyn Component>>);
        root.add_child(Rc::clone(&question_text) as Rc<RefCell<dyn Component>>);
        let (list_container, list_child) = container_child(Container::new());
        root.add_child(list_child);
        let (own_answer_container, own_child) = container_child(Container::new());
        root.add_child(own_child);
        let (submit_container, submit_child) = container_child(Container::new());
        root.add_child(submit_child);
        root.add_child(Rc::clone(&notice_text) as Rc<RefCell<dyn Component>>);
        let submit_region = Rc::new(RefCell::new(MouseRegion::new(
            Rc::clone(&submit_text) as Rc<RefCell<dyn Component>>,
            {
                let pending = Rc::clone(&pending);
                Box::new(move |event: &TuiMouseEvent| {
                    click_region(&pending, AskUserAction::ClickSubmit, event)
                })
            },
        )));
        root.add_child(submit_region);
        root.add_child(Rc::clone(&hints_text) as Rc<RefCell<dyn Component>>);
        root.add_child(text_child_spacer(1));
        root.add_child(component_child(AskUserBorder { theme: options.theme.clone() }));

        let mut component = Self {
            state,
            done,
            theme: options.theme.clone(),
            own_answer_input,
            comment_input,
            countdown: None,
            timeout_ms,
            get_deadline_at_ms: options.get_deadline_at_ms.take(),
            on_progress: options.on_progress.take(),
            root,
            title_text,
            tab_text,
            question_text,
            list_container,
            own_answer_container,
            submit_container,
            notice_text,
            submit_text,
            hints_text,
            pending,
            countdown_label: String::new(),
            settled: false,
            focused: false,
        };

        if let Some(draft) = options.initial_draft.clone() {
            component.state.restore_draft(draft);
            if let Some(comment) = component.state.comment.clone() {
                component.comment_input.borrow_mut().set_value(comment);
            }
        }
        if let Some(index) = options.initial_question_index {
            component.state.jump_to_question(index);
        }
        if timeout_ms > 0 || component.get_deadline_at_ms.is_some() {
            let external = component.get_deadline_at_ms.as_ref().map(|f| f());
            component.countdown = Some(AskUserCountdown::new(timeout_ms, options.now_ms, external));
        }
        component.update_all();
        component.tick(options.now_ms);
        component
    }

    pub fn tick(&mut self, now_ms: u64) {
        let Some(countdown) = &mut self.countdown else {
            return;
        };
        let external = self.get_deadline_at_ms.as_ref().map(|f| f());
        let Some((remaining, expired)) = countdown.tick(now_ms, external) else {
            return;
        };
        let label = format_countdown_label(remaining as f64);
        if label != self.countdown_label {
            self.countdown_label = label;
            self.update_title();
        }
        if expired {
            let timeout_ms = self.timeout_ms;
            self.finish(QuestionStatus::TimedOut, Some(timeout_ms));
        }
    }

    pub fn click_option(&mut self, index: usize, submit_single_question: bool) {
        let question = self.state.active_question().clone();
        let Some(option) = question.options.get(index) else {
            return;
        };
        let immediate = submit_single_question
            && self.state.request.questions.len() == 1
            && !question.multi_select;
        self.state.highlight_index = index;
        self.state.activate_option(&question.id, &option.label);
        if !question.multi_select && !immediate {
            self.state.advance();
        }
        self.emit_progress();
        self.update_all();
        if immediate {
            self.attempt_submit();
        }
    }

    pub fn open_own_answer(&mut self, initial_text: Option<&str>) {
        self.state.focus = QuestionFocus::OwnAnswer;
        let id = self.state.active_question().id.clone();
        let existing = self.state.text_for(&id).unwrap_or("").to_string();
        {
            let mut input = self.own_answer_input.borrow_mut();
            input.set_value("");
            if !existing.is_empty() {
                input.handle_input(&existing);
            }
            if let Some(initial_text) = initial_text {
                input.handle_input(initial_text);
            }
        }
        self.update_all();
    }

    pub fn commit_own_answer(&mut self) {
        let id = self.state.active_question().id.clone();
        let value = self.own_answer_input.borrow().get_value().to_string();
        self.state.set_own_answer(&id, &value);
        self.own_answer_input.borrow_mut().set_value("");
        self.emit_progress();
    }

    pub fn attempt_submit(&mut self) {
        self.state.comment = Some(self.comment_input.borrow().get_value().to_string());
        let outcome = self.state.submit_outcome(self.state.notice.is_some());
        match outcome {
            None => {
                self.state.notice = Some(NOT_ANSWERED_NOTICE.to_string());
                self.update_all();
            }
            Some(status) => {
                self.state.accept_partial_submit();
                self.finish(status, None);
            }
        }
    }

    pub fn emit_progress(&mut self) {
        let mut draft = self.state.refresh_draft();
        if self.state.focus == QuestionFocus::OwnAnswer {
            let live = self.own_answer_input.borrow().get_value().trim().to_string();
            if !live.is_empty() {
                draft.answers.insert(
                    self.state.active_question().id.clone(),
                    QuestionAnswer {
                        selected: Vec::new(),
                        text: Some(live),
                    },
                );
            }
        }
        if let Some(on_progress) = &mut self.on_progress {
            on_progress(&draft);
        }
    }

    pub fn finish(&mut self, status: QuestionStatus, auto_resolved_after_ms: Option<u64>) {
        if self.settled {
            return;
        }
        self.settled = true;
        if let Some(countdown) = &mut self.countdown {
            countdown.dispose();
        }
        self.state.comment = Some(self.comment_input.borrow().get_value().to_string());
        let response = self.state.build_response(status, auto_resolved_after_ms);
        (self.done)(response);
    }

    fn click_submit(&mut self) {
        if self.state.focus == QuestionFocus::Submit {
            self.attempt_submit();
        } else {
            self.state.enter_submit();
            self.update_all();
        }
    }

    fn apply_focus_flags(&mut self) {
        let own_answer = self.focused && self.state.focus == QuestionFocus::OwnAnswer;
        self.own_answer_input.borrow_mut().set_focused(own_answer);
        let comment = self.focused && self.state.is_comment_focused();
        self.comment_input.borrow_mut().set_focused(comment);
    }

    fn update_title(&mut self) {
        let line = render_title(&self.theme, &self.countdown_label);
        self.title_text.borrow_mut().set_text(line);
    }

    fn drain_pending(&mut self) {
        loop {
            let action = self.pending.borrow_mut().pop();
            match action {
                Some(AskUserAction::ClickOption(index)) => self.click_option(index, false),
                Some(AskUserAction::OpenOwnAnswer) => self.open_own_answer(None),
                Some(AskUserAction::ClickSubmit) => self.click_submit(),
                None => break,
            }
        }
        let clicked = self.tab_text.borrow_mut().clicked.take();
        if let Some(index) = clicked {
            if index == self.state.request.questions.len() {
                self.click_submit();
            } else {
                self.state.jump_to_question(index);
                self.update_all();
            }
        }
    }

    pub(crate) fn update_all(&mut self) {
        self.update_title();
        self.apply_focus_flags();
        let tabs = render_tab_labels(&self.theme, &self.state);
        self.tab_text.borrow_mut().set_tabs(tabs);
        let question_line = if self.state.focus == QuestionFocus::Submit {
            "Review your answers".to_string()
        } else {
            render_question_line(&self.theme, self.state.active_question())
        };
        self.question_text.borrow_mut().set_text(question_line);

        let lines = if self.state.focus == QuestionFocus::Submit {
            render_submit_summary(&self.theme, &self.state)
        } else {
            render_question_list(&self.theme, &self.state)
        };
        let option_count = self.state.active_question().options.len();
        let option_descriptions: Vec<bool> = self
            .state
            .active_question()
            .options
            .iter()
            .map(|option| option.description.as_ref().is_some_and(|text| !text.is_empty()))
            .collect();
        {
            let mut list = self.list_container.borrow_mut();
            list.clear();
            let mut option_index = 0usize;
            let mut description = false;
            for line in lines {
                let child = text_child(Text::with_padding(line, 1, 0));
                if self.state.focus == QuestionFocus::Submit || description {
                    list.add_child(child);
                    description = false;
                } else if option_index < option_count {
                    let index = option_index;
                    option_index += 1;
                    list.add_child(mouse_region(child, self.pending.clone(), AskUserAction::ClickOption(index)));
                    description = option_descriptions[index];
                } else {
                    list.add_child(mouse_region(
                        child,
                        self.pending.clone(),
                        AskUserAction::OpenOwnAnswer,
                    ));
                }
            }
        }

        {
            let mut own = self.own_answer_container.borrow_mut();
            own.clear();
            if self.state.focus == QuestionFocus::OwnAnswer {
                own.add_child(text_child(Text::with_padding(
                    render_own_answer_label(&self.theme),
                    1,
                    0,
                )));
                own.add_child(Rc::clone(&self.own_answer_input) as Rc<RefCell<dyn Component>>);
            }
        }

        {
            let mut submit = self.submit_container.borrow_mut();
            submit.clear();
            if self.state.focus == QuestionFocus::Submit {
                submit.add_child(text_child_spacer(1));
                submit.add_child(text_child(Text::with_padding(
                    render_comment_label(&self.theme),
                    1,
                    0,
                )));
                submit.add_child(Rc::clone(&self.comment_input) as Rc<RefCell<dyn Component>>);
            }
        }

        let notice = render_notice(&self.theme, self.state.notice.as_deref());
        self.notice_text.borrow_mut().set_text(notice);
        let submit_line = render_submit_line(&self.theme, &self.state);
        self.submit_text.borrow_mut().set_text(submit_line);
        let hints = render_hints_line(&self.theme, &self.state);
        self.hints_text.borrow_mut().set_text(hints);
    }
}

fn text_child_spacer(lines: usize) -> Rc<RefCell<dyn Component>> {
    Rc::new(RefCell::new(Spacer::new(lines)))
}

fn click_region(
    pending: &Rc<RefCell<Vec<AskUserAction>>>,
    action: AskUserAction,
    event: &TuiMouseEvent,
) -> Option<TuiMouseEventResult> {
    if !is_question_mouse_action(event) || event.y != 0 {
        return None;
    }
    if event.event_type == TuiMouseEventType::Click {
        pending.borrow_mut().push(action);
    }
    Some(TuiMouseEventResult {
        handled: true,
        focus: true,
        ..Default::default()
    })
}

fn mouse_region(
    child: Rc<RefCell<dyn Component>>,
    pending: Rc<RefCell<Vec<AskUserAction>>>,
    action: AskUserAction,
) -> Rc<RefCell<dyn Component>> {
    Rc::new(RefCell::new(MouseRegion::new(
        child,
        Box::new(move |event: &TuiMouseEvent| click_region(&pending, action, event)),
    )))
}

impl Component for AskUserQuestionComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.root.render(width)
    }

    fn handle_input(&mut self, data: &str) {
        super::ask_user_question_keys::handle_ask_user_key_input(self, data);
    }

    fn has_input_handler(&self) -> bool {
        true
    }

    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        if self.settled || !is_question_mouse_action(event) {
            return None;
        }
        let result = self.root.handle_mouse(event);
        self.drain_pending();
        result
    }

    fn focusable_get(&self) -> Option<bool> {
        Some(self.focused)
    }

    fn focusable_set(&mut self, focused: bool) {
        self.focused = focused;
        self.apply_focus_flags();
    }

    fn invalidate(&mut self) {
        for child in &self.root.children {
            child.borrow_mut().invalidate();
        }
    }

    fn dispose(&mut self) {
        if let Some(countdown) = &mut self.countdown {
            countdown.dispose();
        }
    }
}

impl Focusable for AskUserQuestionComponent {
    fn focused(&self) -> bool {
        self.focused
    }

    fn set_focused(&mut self, value: bool) {
        self.focused = value;
        self.apply_focus_flags();
    }
}
