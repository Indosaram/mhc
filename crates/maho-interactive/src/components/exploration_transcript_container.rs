//! Port of `components/exploration-transcript-container.ts`.
//!
//! senpi tests child identity with `instanceof`; Rust components erase their concrete type behind
//! `dyn Component`, so this container keeps the typed handle alongside each child instead.
use std::cell::RefCell;
use std::rc::Rc;

use maho_tui::tui::{Component, TuiMouseEvent, TuiMouseEventResult};
use serde_json::Value;

use super::assistant_message::AssistantMessageComponent;
use super::custom_entry::CustomEntryComponent;
use super::exploration_call::{ExplorationCall, exploration_call};
use super::exploration_group::ExplorationGroup;
use super::exploration_rules::project_rules_of_call;
use super::progressive_transcript_container::{ProgressiveTranscriptContainer, ProgressiveTranscriptOptions};
use super::tool_execution::ToolExecutionComponent;
use crate::theme::Theme;

pub enum TranscriptChild {
    Tool(Rc<RefCell<ToolExecutionComponent>>),
    Assistant(Rc<RefCell<AssistantMessageComponent>>),
    Entry(Rc<RefCell<CustomEntryComponent>>),
    Other(Rc<RefCell<dyn Component>>),
}

impl Clone for TranscriptChild {
    fn clone(&self) -> Self {
        match self {
            Self::Tool(component) => Self::Tool(Rc::clone(component)),
            Self::Assistant(component) => Self::Assistant(Rc::clone(component)),
            Self::Entry(component) => Self::Entry(Rc::clone(component)),
            Self::Other(component) => Self::Other(Rc::clone(component)),
        }
    }
}

impl TranscriptChild {
    fn component(&self) -> Rc<RefCell<dyn Component>> {
        match self {
            Self::Tool(component) => Rc::clone(component) as Rc<RefCell<dyn Component>>,
            Self::Assistant(component) => Rc::clone(component) as Rc<RefCell<dyn Component>>,
            Self::Entry(component) => Rc::clone(component) as Rc<RefCell<dyn Component>>,
            Self::Other(component) => Rc::clone(component),
        }
    }
}

pub type GroupEntry = (Rc<RefCell<ToolExecutionComponent>>, Rc<RefCell<ExplorationGroup>>);

/// Paint-path hydration must not lay out the full warm chunk (100) before the frame returns.
const PAINT_PATH_WARM_CAP: usize = 8;

pub struct ExplorationTranscriptContainer {
    children: Vec<TranscriptChild>,
    display: ProgressiveTranscriptContainer,
    theme: Theme,
    groups: Vec<GroupEntry>,
    projection_ptrs: Vec<usize>,
    assistant_detail: Vec<Option<bool>>,
    entry_snapshot: Vec<Option<Value>>,
    /// Times `render` rebuilt the exploration projection. A reused paint does not increment it.
    #[cfg(test)]
    projection_walks: u64,
}

fn child_ptr(child: &TranscriptChild) -> usize {
    match child {
        TranscriptChild::Tool(component) => Rc::as_ptr(component) as *const () as usize,
        TranscriptChild::Assistant(component) => Rc::as_ptr(component) as *const () as usize,
        TranscriptChild::Entry(component) => Rc::as_ptr(component) as *const () as usize,
        TranscriptChild::Other(component) => Rc::as_ptr(component) as *const () as usize,
    }
}

impl ExplorationTranscriptContainer {
    pub fn new(options: ProgressiveTranscriptOptions, theme: Theme) -> Self {
        Self {
            children: Vec::new(),
            display: ProgressiveTranscriptContainer::new(options),
            theme,
            groups: Vec::new(),
            projection_ptrs: Vec::new(),
            assistant_detail: Vec::new(),
            entry_snapshot: Vec::new(),
            #[cfg(test)]
            projection_walks: 0,
        }
    }

    pub fn is_fully_hydrated(&self) -> bool {
        self.display.is_fully_hydrated()
    }

    pub fn add_child(&mut self, child: TranscriptChild) {
        self.children.push(child);
    }

    /// A plain component child (senpi `addChild` with a non-exploration component).
    pub fn add_component(&mut self, child: Rc<RefCell<dyn Component>>) {
        self.add_child(TranscriptChild::Other(child));
    }

    pub fn children(&self) -> &[TranscriptChild] {
        &self.children
    }

    pub fn children_mut(&mut self) -> &mut Vec<TranscriptChild> {
        &mut self.children
    }

    /// senpi `clear`: drop the projection and its groups, re-arm hydration, then clear the children.
    pub fn clear(&mut self) {
        self.display.children_mut().clear();
        self.display.rearm();
        self.groups.clear();
        self.forget_projection();
        for child in self.children.drain(..) {
            child.component().borrow_mut().dispose();
        }
    }

    /// One bounded hydration step of the progressive display (senpi's `setImmediate` warm chunk).
    ///
    /// The host calls this from the paint tick. Moving the watermark by the full `warm_chunk_size`
    /// (100) would make that paint lay out 100 extra rows, so the paint path advances at most
    /// [`PAINT_PATH_WARM_CAP`] children and does not render them itself.
    pub fn warm_next_chunk(&mut self) {
        self.display.warm_next_chunk_limited(PAINT_PATH_WARM_CAP);
    }

    fn forget_projection(&mut self) {
        self.projection_ptrs.clear();
        self.assistant_detail.clear();
        self.entry_snapshot.clear();
    }

    /// Child set and the inputs the projection reads are unchanged, so the last grouped children
    /// are still the ones `display` holds.
    fn projection_current(&self) -> bool {
        if self.projection_ptrs.len() != self.children.len() {
            return false;
        }
        for (index, child) in self.children.iter().enumerate() {
            let Some(cached_ptr) = self.projection_ptrs.get(index) else {
                return false;
            };
            if child_ptr(child) != *cached_ptr {
                return false;
            }
            match child {
                TranscriptChild::Tool(card) => {
                    if card.borrow().exploration_cache.borrow().is_none() {
                        return false;
                    }
                }
                TranscriptChild::Assistant(card) => {
                    let detail = card.borrow().is_exploration_detail();
                    if self.assistant_detail.get(index).copied() != Some(Some(detail)) {
                        return false;
                    }
                }
                TranscriptChild::Entry(card) => {
                    let current = &card.borrow().custom_entry;
                    match self.entry_snapshot.get(index) {
                        Some(Some(cached)) if cached == current => {}
                        _ => return false,
                    }
                }
                TranscriptChild::Other(_) => {}
            }
        }
        true
    }

    fn remember_projection(&mut self) {
        self.assistant_detail = self
            .children
            .iter()
            .map(|child| match child {
                TranscriptChild::Assistant(card) => Some(card.borrow().is_exploration_detail()),
                _ => None,
            })
            .collect();
        self.entry_snapshot = self
            .children
            .iter()
            .map(|child| match child {
                TranscriptChild::Entry(card) => Some(card.borrow().custom_entry.clone()),
                _ => None,
            })
            .collect();
        self.projection_ptrs = self.children.iter().map(child_ptr).collect();
    }

    fn group_for(
        groups: &mut Vec<GroupEntry>,
        theme: &Theme,
        card: &Rc<RefCell<ToolExecutionComponent>>,
    ) -> Rc<RefCell<ExplorationGroup>> {
        if let Some((_, group)) = groups.iter().find(|(existing, _)| Rc::ptr_eq(existing, card)) {
            return Rc::clone(group);
        }
        let group = Rc::new(RefCell::new(ExplorationGroup::new(theme.clone())));
        groups.push((Rc::clone(card), Rc::clone(&group)));
        group
    }
}

type GroupCalls = Vec<(Rc<RefCell<ToolExecutionComponent>>, ExplorationCall)>;

/// Hand the accumulated members to the open group. senpi calls `setMembers` after every added
/// call or rule; only the last call is observable, so it runs once when the group closes.
fn flush_group(
    group: &mut Option<Rc<RefCell<ExplorationGroup>>>,
    members: &mut Vec<Rc<RefCell<dyn Component>>>,
    calls: &mut GroupCalls,
    rules: &mut Vec<String>,
    members_at_set: usize,
) {
    if let Some(group) = group.take() {
        members.truncate(members_at_set);
        group.borrow_mut().set_members(
            std::mem::take(members),
            std::mem::take(calls),
            std::mem::take(rules),
        );
    }
}

impl Component for ExplorationTranscriptContainer {
    fn render(&mut self, width: usize) -> Vec<String> {
        if self.projection_current() {
            // Child lines are painted by `display`. Skipping the walk does not skip those renders:
            // tool cards and Other children mutate in place with no container invalidation.
            return self.display.render(width);
        }
        #[cfg(test)]
        {
            self.projection_walks = self.projection_walks.saturating_add(1);
        }
        let mut projected: Vec<Rc<RefCell<dyn Component>>> = Vec::new();
        let mut group: Option<Rc<RefCell<ExplorationGroup>>> = None;
        let mut members: Vec<Rc<RefCell<dyn Component>>> = Vec::new();
        let mut calls: GroupCalls = Vec::new();
        let mut rules: Vec<String> = Vec::new();
        // Member count at the group's last senpi `setMembers`; exploration-detail assistants
        // appended after it are not part of the group's members.
        let mut members_at_set = 0;

        for child in &self.children {
            if let TranscriptChild::Tool(card) = child {
                let call = exploration_call(&card.borrow());
                if let Some(call) = call {
                    if group.is_none() {
                        let existing = Self::group_for(&mut self.groups, &self.theme, card);
                        group = Some(Rc::clone(&existing));
                        projected.push(existing as Rc<RefCell<dyn Component>>);
                    }
                    members.push(child.component());
                    calls.push((Rc::clone(card), call));
                    members_at_set = members.len();
                    continue;
                }
            }
            if group.is_some()
                && let TranscriptChild::Assistant(assistant) = child
                && assistant.borrow().is_exploration_detail()
            {
                members.push(child.component());
                continue;
            }
            if group.is_some()
                && let TranscriptChild::Entry(entry) = child
                && let Some(projected_rules) = project_rules_of_call(&entry.borrow(), &calls)
            {
                members.push(child.component());
                rules.extend(projected_rules);
                members_at_set = members.len();
                continue;
            }
            flush_group(&mut group, &mut members, &mut calls, &mut rules, members_at_set);
            projected.push(child.component());
        }
        flush_group(&mut group, &mut members, &mut calls, &mut rules, members_at_set);

        self.remember_projection();
        *self.display.children_mut() = projected;
        self.display.render(width)
    }

    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.display.handle_mouse(event)
    }

    fn invalidate(&mut self) {
        self.forget_projection();
        self.display.invalidate();
    }

    fn dispose(&mut self) {
        self.display.dispose();
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    use maho_tui::tui::Component;

    use super::{ExplorationTranscriptContainer, TranscriptChild, PAINT_PATH_WARM_CAP};
    use crate::components::progressive_transcript_container::ProgressiveTranscriptOptions;
    use crate::theme::{ColorMode, Theme};

    struct Probe {
        hits: Rc<Cell<usize>>,
        text: Rc<Cell<&'static str>>,
    }

    impl Component for Probe {
        fn render(&mut self, _width: usize) -> Vec<String> {
            self.hits.set(self.hits.get().saturating_add(1));
            vec![self.text.get().to_owned()]
        }

        fn invalidate(&mut self) {}
    }

    fn theme() -> Theme {
        Theme::builtin("dark", ColorMode::Color256).expect("builtin theme")
    }

    fn container() -> ExplorationTranscriptContainer {
        ExplorationTranscriptContainer::new(
            ProgressiveTranscriptOptions {
                tail_budget: 60,
                warm_chunk_size: 100,
                request_render: Rc::new(|| {}),
            },
            theme(),
        )
    }

    /// R3: an unchanged child set must not regroup. In-place text still paints, because tool cards
    /// and Other children mutate without `chat.invalidate`.
    #[test]
    fn second_render_skips_regrouping_and_shows_in_place_text() {
        let hits = Rc::new(Cell::new(0usize));
        let text = Rc::new(Cell::new("before"));
        let mut transcript = container();
        for _ in 0..40 {
            transcript.add_child(TranscriptChild::Other(Rc::new(RefCell::new(Probe {
                hits: Rc::clone(&hits),
                text: Rc::clone(&text),
            }))));
        }
        let first = transcript.render(48);
        assert_eq!(transcript.projection_walks, 1, "the first paint builds the projection");
        assert_eq!(transcript.groups.len(), 0);
        assert!(first.iter().all(|line| line == "before"));
        text.set("after");
        let second = transcript.render(48);
        assert_eq!(second, vec!["after".to_owned(); 40], "in-place text must reach the second paint");
        assert_eq!(transcript.projection_walks, 1, "second paint regrouped; group_for ran again");
        assert_eq!(transcript.groups.len(), 0, "groups changed without a child-set change");
        assert!(hits.get() > 40, "second paint did not render the children");
    }

    /// R3: `warm_next_chunk` runs on the paint tick. It must not lay out the full warm chunk (100)
    /// before the frame returns. On a8ac6196 that call renders 100 children.
    #[test]
    fn unchanged_paint_does_not_warm_the_full_chunk() {
        let hits = Rc::new(Cell::new(0usize));
        let text = Rc::new(Cell::new("row"));
        let mut transcript = container();
        for _ in 0..200 {
            transcript.add_child(TranscriptChild::Other(Rc::new(RefCell::new(Probe {
                hits: Rc::clone(&hits),
                text: Rc::clone(&text),
            }))));
        }
        let first = transcript.render(48);
        let after_paint = hits.get();
        assert_eq!(after_paint, 60, "the first paint is the tail only");
        let walks = transcript.projection_walks;
        transcript.warm_next_chunk();
        let warmed = hits.get().saturating_sub(after_paint);
        assert!(
            warmed <= PAINT_PATH_WARM_CAP,
            "paint-path warm laid out {warmed} children, cap is {PAINT_PATH_WARM_CAP}"
        );
        assert_eq!(transcript.projection_walks, walks, "warm must not regroup");
        let second = transcript.render(48);
        assert!(second.len() > first.len(), "the capped warm must still reveal older lines");
        assert!(second.ends_with(first.as_slice()), "warming must extend the previous tail");
    }
}
