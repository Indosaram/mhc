//! Port of `interactive-mode.ts`'s `pendingQuestions` / `pendingOrder` / `shownQuestionId`
//! / `questionSurface` / `composerDestination`.

use std::collections::BTreeMap;

use crate::components::ask_user_question_state::{QuestionDraft, QuestionRequest};

/// senpi's `questionSurface`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum QuestionSurface {
    #[default]
    Collapsed,
    List,
    Expanded,
}

/// senpi's `composerDestination`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ComposerDestination {
    #[default]
    Chat,
    Answer { request_id: String },
}

/// senpi's `AsyncQuestionState`. `replies` holds every completion sender, so a duplicate
/// `question` call for one request id resolves with the first when the question settles.
pub struct PendingQuestion {
    pub request: QuestionRequest,
    pub draft: QuestionDraft,
    pub timeout_ms: u64,
    pub asked_at_ms: u64,
    pub get_deadline_at_ms: Option<std::rc::Rc<dyn Fn() -> u64>>,
    pub on_progress: Option<std::rc::Rc<std::cell::RefCell<Box<dyn FnMut(&QuestionDraft)>>>>,
    pub replies: Vec<tokio::sync::oneshot::Sender<maho_ext_api::QuestionResponse>>,
}

/// Translate producer wall-clock deadlines into the terminal's monotonic clock once at attach.
/// The producer remains the expiry owner; the callback is only a live display source.
pub(crate) fn question_deadline(options: &maho_ext_api::QuestionOptions, now_ms: u64) -> Option<std::rc::Rc<dyn Fn() -> u64>> {
    let wall_now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX));
    question_deadline_at(options, now_ms, wall_now)
}

fn question_deadline_at(options: &maho_ext_api::QuestionOptions, now_ms: u64, wall_now: u64) -> Option<std::rc::Rc<dyn Fn() -> u64>> {
    let deadline = options.get_deadline_at_ms.clone()?;
    let hard = options.hard_deadline_at_ms;
    Some(std::rc::Rc::new(move || {
        let absolute = deadline();
        let absolute = hard.map_or(absolute, |hard| absolute.min(hard));
        if absolute >= wall_now { now_ms.saturating_add(absolute - wall_now) }
        else { now_ms.saturating_sub(wall_now - absolute) }
    }))
}

pub(crate) fn question_draft(draft: maho_ext_api::QuestionDraft) -> QuestionDraft {
    QuestionDraft { comment: draft.comment, answers: draft.answers.unwrap_or_default().into_iter().map(|(id, answer)| (id, crate::components::ask_user_question_state::QuestionAnswer { selected: answer.selected, text: answer.text })).collect() }
}

impl PendingQuestion {
    /// senpi's `unansweredIds(request, draft)` over this question's draft.
    pub fn unanswered(&self) -> Vec<String> {
        crate::components::ask_user_async_widget::unanswered_ids(&self.request, &self.draft)
    }

    pub fn deadline_at_ms(&self) -> u64 {
        self.get_deadline_at_ms
            .as_ref()
            .map(|deadline| deadline())
            .unwrap_or_else(|| self.asked_at_ms.saturating_add(self.timeout_ms))
    }
}

/// senpi's pending-question map plus its ordering and surface bookkeeping.
#[derive(Default)]
pub struct QuestionRegistry {
    pending: BTreeMap<String, PendingQuestion>,
    order: Vec<String>,
    shown_id: Option<String>,
    pub surface: QuestionSurface,
    pub composer_destination: ComposerDestination,
}

impl QuestionRegistry {
    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    pub fn contains(&self, request_id: &str) -> bool {
        self.pending.contains_key(request_id)
    }

    pub fn get(&self, request_id: &str) -> Option<&PendingQuestion> {
        self.pending.get(request_id)
    }

    pub fn get_mut(&mut self, request_id: &str) -> Option<&mut PendingQuestion> {
        self.pending.get_mut(request_id)
    }

    /// senpi's `pendingOrder`.
    pub fn order(&self) -> &[String] {
        &self.order
    }

    /// senpi's `shownQuestion`: the request the collapsed widget or expanded component shows.
    pub fn shown(&self) -> Option<&PendingQuestion> {
        self.shown_id.as_ref().and_then(|id| self.pending.get(id))
    }

    pub fn shown_mut(&mut self) -> Option<&mut PendingQuestion> {
        let id = self.shown_id.clone()?;
        self.pending.get_mut(&id)
    }

    pub fn shown_id(&self) -> Option<&str> {
        self.shown_id.as_deref()
    }

    /// `false` when the request is already pending, matching senpi's shared completion.
    pub fn show(&mut self, question: PendingQuestion) -> bool {
        let request_id = question.request.request_id.clone();
        if self.pending.contains_key(&request_id) {
            return false;
        }
        self.pending.insert(request_id.clone(), question);
        self.order.push(request_id.clone());
        self.shown_id.get_or_insert(request_id);
        true
    }

    /// senpi's `AsyncQuestionState.finish`; returns the removed question for its completion.
    pub fn finish(&mut self, request_id: &str) -> Option<PendingQuestion> {
        let removed = self.pending.remove(request_id)?;
        self.order.retain(|id| id != request_id);
        if self.composer_destination == (ComposerDestination::Answer { request_id: request_id.to_owned() }) {
            self.composer_destination = ComposerDestination::Chat;
        }
        if self.order.is_empty() && self.surface == QuestionSurface::List {
            self.surface = QuestionSurface::Collapsed;
        }
        if self.shown_id.as_deref() == Some(request_id) {
            if self.surface == QuestionSurface::Expanded {
                self.surface = QuestionSurface::Collapsed;
            }
            self.shown_id = self.order.first().cloned();
        }
        Some(removed)
    }

    /// senpi's `cyclePendingQuestion`; `false` with fewer than two pending questions.
    pub fn cycle(&mut self) -> bool {
        if self.order.len() < 2 {
            return false;
        }
        let current = self.shown_id.clone().unwrap_or_default();
        let index = self.order.iter().position(|id| id == &current).unwrap_or(0);
        self.shown_id = Some(self.order[(index + 1) % self.order.len()].clone());
        true
    }

    /// senpi's `expandPendingQuestion(requestId)`.
    pub fn set_shown(&mut self, request_id: &str) -> bool {
        if !self.pending.contains_key(request_id) {
            return false;
        }
        self.shown_id = Some(request_id.to_owned());
        true
    }

    /// senpi's `/answer <n>`: the 1-based position in `pendingOrder`.
    pub fn by_number(&self, number: usize) -> Option<&str> {

        number.checked_sub(1).and_then(|index| self.order.get(index)).map(String::as_str)
    }

    /// senpi's `setComposerReply(requestId?)`.
    pub fn set_composer_reply(&mut self, request_id: Option<&str>) {
        self.composer_destination = match request_id {
            Some(request_id) if self.pending.contains_key(request_id) => {
                ComposerDestination::Answer { request_id: request_id.to_owned() }
            }
            _ => ComposerDestination::Chat,
        };
    }

    pub fn composer_request_id(&self) -> Option<&str> {
        match &self.composer_destination {
            ComposerDestination::Answer { request_id } => Some(request_id),
            ComposerDestination::Chat => None,
        }
    }
}

/// senpi's `/answer` list row: `<header> · <countdown> ago · <countdown> remaining`.
pub fn answer_list_label(question: &PendingQuestion, now_ms: u64) -> String {
    let header = question
        .request
        .questions
        .first()
        .map_or_else(|| "Question".to_owned(), |question| question.header.clone());
    let elapsed = now_ms.saturating_sub(question.asked_at_ms);
    let remaining = question.deadline_at_ms().saturating_sub(now_ms);
    format!(
        "{header} · {} ago · {} remaining",
        crate::components::ask_user_question_state::format_countdown_label(elapsed as f64),
        crate::components::ask_user_question_state::format_countdown_label(remaining as f64)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::ask_user_question_state::{Question, QuestionOption};

    fn request(id: &str, wait_for_answer: bool) -> QuestionRequest {
        QuestionRequest {
            request_id: id.into(),
            wait_for_answer,
            timeout_ms: 60_000,
            questions: vec![Question { id: "item".into(), header: format!("Header {id}"), question: "Choose".into(), options: vec![QuestionOption { label: "A".into(), description: None }], multi_select: false }],
        }
    }

    fn pending(id: &str) -> PendingQuestion {
        let (reply, receiver) = tokio::sync::oneshot::channel();
        std::mem::forget(receiver);
        PendingQuestion { request: request(id, false), draft: QuestionDraft::default(), timeout_ms: 60_000, asked_at_ms: 0, get_deadline_at_ms: None, on_progress: None, replies: vec![reply] }
    }

    #[test]
    fn duplicates_share_the_existing_question() {
        let mut registry = QuestionRegistry::default();
        assert!(registry.show(pending("q1")));
        assert!(!registry.show(pending("q1")), "a duplicate request id is not shown twice");
        assert_eq!(registry.len(), 1);
        assert_eq!(registry.shown_id(), Some("q1"));
    }

    #[test]
    fn finish_hands_the_surface_to_the_next_pending_question() {
        let mut registry = QuestionRegistry::default();
        registry.show(pending("q1"));
        registry.show(pending("q2"));
        registry.surface = QuestionSurface::Expanded;
        registry.set_composer_reply(Some("q1"));
        assert!(registry.finish("q1").is_some());
        assert_eq!(registry.order(), ["q2"]);
        assert_eq!(registry.shown_id(), Some("q2"));
        assert_eq!(registry.surface, QuestionSurface::Collapsed, "expanded surface collapses with its question");
        assert_eq!(registry.composer_destination, ComposerDestination::Chat, "the reply target is cleared");
        assert!(registry.finish("missing").is_none());
    }

    #[test]
    fn cycle_rotates_only_with_two_pending_and_list_survives() {
        let mut registry = QuestionRegistry::default();
        registry.show(pending("q1"));
        assert!(!registry.cycle(), "cycling needs two pending questions");
        registry.show(pending("q2"));
        registry.surface = QuestionSurface::List;
        assert!(registry.cycle());
        assert_eq!(registry.shown_id(), Some("q2"));
        assert!(registry.cycle());
        assert_eq!(registry.shown_id(), Some("q1"));
        registry.finish("q1");
        assert_eq!(registry.surface, QuestionSurface::List, "a non-empty list stays open");
        registry.finish("q2");
        assert_eq!(registry.surface, QuestionSurface::Collapsed, "the empty list collapses");
    }

    #[test]
    fn numbering_addresses_arrival_order_and_rejects_zero() {
        let mut registry = QuestionRegistry::default();
        registry.show(pending("a"));
        registry.show(pending("b"));
        assert_eq!(registry.by_number(1), Some("a"));
        assert_eq!(registry.by_number(2), Some("b"));
        assert_eq!(registry.by_number(3), None);
        assert_eq!(registry.by_number(0), None);
    }

    #[test]
    fn composer_reply_targets_only_a_pending_request() {
        let mut registry = QuestionRegistry::default();
        registry.show(pending("q1"));
        registry.set_composer_reply(Some("q1"));
        assert_eq!(registry.composer_request_id(), Some("q1"));
        registry.set_composer_reply(Some("ghost"));
        assert_eq!(registry.composer_request_id(), None, "an unknown request falls back to chat");
        registry.set_composer_reply(Some("q1"));
        registry.set_composer_reply(None);
        assert_eq!(registry.composer_request_id(), None);
    }

    #[test]
    fn unanswered_ids_follow_the_draft_and_labels_render_countdowns() {
        let mut question = pending("q1");
        question.asked_at_ms = 1_000;
        assert_eq!(question.unanswered(), ["item"]);
        assert_eq!(question.deadline_at_ms(), 61_000);
        question.draft.answers.insert("item".into(), crate::components::ask_user_question_state::QuestionAnswer { selected: vec!["A".into()], text: None });
        assert!(question.unanswered().is_empty());
        let label = answer_list_label(&question, 11_000);
        assert!(label.starts_with("Header q1 · 00:10 ago · 00:50 remaining"), "label: {label}");
    }

    #[test]
    fn live_deadline_tracks_idle_updates_without_moving_the_hard_cap() {
        let live = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(11_000));
        let source = live.clone();
        let options = maho_ext_api::QuestionOptions { get_deadline_at_ms: Some(std::sync::Arc::new(move || source.load(std::sync::atomic::Ordering::Relaxed))), hard_deadline_at_ms: Some(15_000), ..Default::default() };
        let deadline = question_deadline_at(&options, 500, 10_000).expect("live deadline");
        assert_eq!(deadline(), 1_500);
        live.store(13_000, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(deadline(), 3_500);
        live.store(20_000, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(deadline(), 5_500);
        let reattached = question_deadline_at(&options, 2_500, 12_000).expect("reattached deadline");
        assert_eq!(reattached(), deadline());
    }

    #[test]
    fn retained_draft_conversion_preserves_answers_and_comment() {
        let draft = question_draft(maho_ext_api::QuestionDraft { answers: Some([("item".into(), maho_ext_api::QuestionAnswer { selected: vec!["A".into()], text: Some("retained".into()) })].into()), comment: Some("comment".into()) });
        assert_eq!(draft.answers["item"].selected, ["A"]);
        assert_eq!(draft.answers["item"].text.as_deref(), Some("retained"));
        assert_eq!(draft.comment.as_deref(), Some("comment"));
    }
}
