//! Port of `components/exploration-transcript-container.ts`.
//!
//! senpi tests child identity with `instanceof`; Rust components erase their concrete type behind
//! `dyn Component`, so this container keeps the typed handle alongside each child instead.
use std::cell::RefCell;
use std::rc::Rc;

use maho_tui::tui::{Component, TuiMouseEvent, TuiMouseEventResult};

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

pub struct ExplorationTranscriptContainer {
    children: Vec<TranscriptChild>,
    display: ProgressiveTranscriptContainer,
    theme: Theme,
    groups: Vec<GroupEntry>,
}

impl ExplorationTranscriptContainer {
    pub fn new(options: ProgressiveTranscriptOptions, theme: Theme) -> Self {
        Self {
            children: Vec::new(),
            display: ProgressiveTranscriptContainer::new(options),
            theme,
            groups: Vec::new(),
        }
    }

    pub fn add_child(&mut self, child: TranscriptChild) {
        self.children.push(child);
    }

    pub fn children(&self) -> &[TranscriptChild] {
        &self.children
    }

    pub fn children_mut(&mut self) -> &mut Vec<TranscriptChild> {
        &mut self.children
    }

    fn group_for(&mut self, card: &Rc<RefCell<ToolExecutionComponent>>) -> Rc<RefCell<ExplorationGroup>> {
        if let Some((_, group)) = self.groups.iter().find(|(existing, _)| Rc::ptr_eq(existing, card)) {
            return Rc::clone(group);
        }
        let group = Rc::new(RefCell::new(ExplorationGroup::new(self.theme.clone())));
        self.groups.push((Rc::clone(card), Rc::clone(&group)));
        group
    }
}

impl Component for ExplorationTranscriptContainer {
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut projected: Vec<Rc<RefCell<dyn Component>>> = Vec::new();
        let mut group: Option<Rc<RefCell<ExplorationGroup>>> = None;
        let mut members: Vec<Rc<RefCell<dyn Component>>> = Vec::new();
        let mut calls: Vec<(Rc<RefCell<ToolExecutionComponent>>, ExplorationCall)> = Vec::new();
        let mut rules: Vec<String> = Vec::new();

        let children: Vec<TranscriptChild> = self.children.clone();
        for child in &children {
            let component = child.component();
            if let TranscriptChild::Tool(card) = child {
                let call = exploration_call(&card.borrow());
                if let Some(call) = call {
                    if group.is_none() {
                        let existing = self.group_for(card);
                        group = Some(Rc::clone(&existing));
                        projected.push(Rc::clone(&existing) as Rc<RefCell<dyn Component>>);
                        members = Vec::new();
                        calls = Vec::new();
                        rules = Vec::new();
                    }
                    members.push(Rc::clone(&component));
                    calls.push((Rc::clone(card), call));
                    if let Some(group) = &group {
                        group.borrow_mut().set_members(members.clone(), calls.clone(), rules.clone());
                    }
                    continue;
                }
            }
            if group.is_some()
                && let TranscriptChild::Assistant(assistant) = child
                && assistant.borrow().is_exploration_detail()
            {
                members.push(Rc::clone(&component));
                continue;
            }
            if group.is_some()
                && let TranscriptChild::Entry(entry) = child
                && let Some(projected_rules) = project_rules_of_call(&entry.borrow(), &calls)
            {
                members.push(Rc::clone(&component));
                rules.extend(projected_rules);
                if let Some(group) = &group {
                    group.borrow_mut().set_members(members.clone(), calls.clone(), rules.clone());
                }
                continue;
            }
            group = None;
            projected.push(component);
        }

        *self.display.children_mut() = projected;
        self.display.render(width)
    }

    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.display.handle_mouse(event)
    }

    fn invalidate(&mut self) {
        self.display.invalidate();
    }

    fn dispose(&mut self) {
        self.display.dispose();
    }
}
