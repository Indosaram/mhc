//! Port of senpi `packages/coding-agent/src/modes/interactive/components/tool-execution-animation.ts`.
//!
//! senpi drives the spinner and todo-strike frames with `setInterval`; this port keeps the frame
//! arithmetic and advances it on explicit host ticks ([`ToolExecutionAnimation::tick`]), like
//! [`super::tool_execution::ToolExecutionComponent::tick`].

use super::todo_strike::{
    TODO_STRIKE_FRAME_INTERVAL_MS, TODO_STRIKE_TOTAL_FRAMES, has_completed_todo_tasks,
};
use super::tool_execution_types::ToolExecutionRenderState;
use crate::tool_progress::read_tool_progress;

pub const PENDING_RENDER_FRAME_INTERVAL_MS: u64 = 80;

/// A card spins while edit-like arguments stream in, or while a partial task or progress result
/// is showing.
pub fn tool_card_spins(tool_name: &str, card: &ToolExecutionRenderState) -> bool {
    let is_streaming_args =
        !card.args_complete && matches!(tool_name, "edit" | "write" | "apply_patch");
    let is_partial_task = card.is_partial && tool_name == "task" && card.result.is_some();
    let is_partial_progress = card.is_partial
        && card
            .result
            .as_ref()
            .and_then(|result| result.details.as_ref())
            .is_some_and(|details| read_tool_progress(details).is_some());
    is_streaming_args || is_partial_task || is_partial_progress
}

/// A finished, successful todo card that completed tasks strikes them through once.
pub fn tool_card_strikes(tool_name: &str, card: &ToolExecutionRenderState) -> bool {
    tool_name == "todo"
        && card.execution_started
        && !card.is_partial
        && card.result.as_ref().is_some_and(|result| {
            !result.is_error
                && result
                    .details
                    .as_ref()
                    .is_some_and(has_completed_todo_tasks)
        })
}

/// senpi `ToolExecutionAnimationHost`.
pub trait ToolExecutionAnimationHost {
    /// The card's output changed: drop its render cache.
    fn invalidate(&mut self);
    /// Rebuild the card's display and ask the terminal for a frame.
    fn redraw(&mut self);
}

/// A tool card's spinner and todo-strike timers and the frame they advance. Each timer is the
/// timestamp of its last fire; [`ToolExecutionAnimation::tick`] fires the elapsed ones.
#[derive(Debug, Default)]
pub struct ToolExecutionAnimation {
    spinner: Option<u64>,
    strike: Option<u64>,
    frame: Option<i64>,
}

impl ToolExecutionAnimation {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn frame(&self) -> Option<i64> {
        self.frame
    }

    pub fn running(&self) -> bool {
        self.spinner.is_some() || self.strike.is_some()
    }

    pub fn spin(&mut self, on: bool, now_ms: u64, host: &mut dyn ToolExecutionAnimationHost) {
        if on {
            self.spinner.get_or_insert(now_ms);
        } else {
            self.stop_spinner(host);
        }
    }

    pub fn strike(&mut self, on: bool, now_ms: u64, host: &mut dyn ToolExecutionAnimationHost) {
        if !on {
            self.stop_strike(host);
            return;
        }
        if self.strike.is_some() {
            return;
        }
        self.frame = Some(0);
        self.strike = Some(now_ms);
    }

    pub fn stop(&mut self, host: &mut dyn ToolExecutionAnimationHost) {
        self.stop_spinner(host);
        self.stop_strike(host);
    }

    /// Fire every interval that elapsed by `now_ms`, in the order senpi's timers would.
    pub fn tick(&mut self, now_ms: u64, host: &mut dyn ToolExecutionAnimationHost) {
        while let Some(last) = self.spinner
            && now_ms.saturating_sub(last) >= PENDING_RENDER_FRAME_INTERVAL_MS
        {
            self.spinner = Some(last + PENDING_RENDER_FRAME_INTERVAL_MS);
            self.frame = Some((self.frame.unwrap_or(-1) + 1) % 10);
            host.invalidate();
            host.redraw();
        }
        while let Some(last) = self.strike
            && now_ms.saturating_sub(last) >= TODO_STRIKE_FRAME_INTERVAL_MS
        {
            self.strike = Some(last + TODO_STRIKE_FRAME_INTERVAL_MS);
            let next = self.frame.unwrap_or(0) + 1;
            if next > TODO_STRIKE_TOTAL_FRAMES {
                self.stop_strike(host);
                return;
            }
            self.frame = Some(next);
            host.invalidate();
            host.redraw();
        }
    }

    fn stop_spinner(&mut self, host: &mut dyn ToolExecutionAnimationHost) {
        if self.spinner.take().is_none() {
            return;
        }
        if self.strike.is_none() {
            self.frame = None;
        }
        host.invalidate();
    }

    fn stop_strike(&mut self, host: &mut dyn ToolExecutionAnimationHost) {
        self.strike = None;
        if self.spinner.is_none() && self.frame.is_some() {
            self.frame = None;
            host.invalidate();
            host.redraw();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use serde_json::json;

    use super::super::tool_execution_types::ToolExecutionResult;
    use super::*;

    #[derive(Default)]
    struct Host {
        invalidations: usize,
        redraws: usize,
    }

    impl ToolExecutionAnimationHost for Host {
        fn invalidate(&mut self) {
            self.invalidations += 1;
        }
        fn redraw(&mut self) {
            self.redraws += 1;
        }
    }

    fn card() -> ToolExecutionRenderState {
        ToolExecutionRenderState {
            args: json!({}),
            execution_started: true,
            args_complete: true,
            is_partial: false,
            expanded: false,
            show_images: false,
            spinner_frame: None,
            result: None,
            is_error: false,
            now_ms: 0.0,
        }
    }

    fn result(details: serde_json::Value, is_error: bool) -> Option<Rc<ToolExecutionResult>> {
        Some(Rc::new(ToolExecutionResult {
            content: Vec::new(),
            details: Some(details),
            is_error,
        }))
    }

    #[test]
    fn cards_spin_for_streaming_edit_args_partial_tasks_and_progress_results() {
        let streaming = ToolExecutionRenderState {
            args_complete: false,
            ..card()
        };
        assert!(tool_card_spins("edit", &streaming));
        assert!(tool_card_spins("apply_patch", &streaming));
        assert!(!tool_card_spins("read", &streaming));

        let partial_task = ToolExecutionRenderState {
            is_partial: true,
            result: result(json!({}), false),
            ..card()
        };
        assert!(tool_card_spins("task", &partial_task));
        assert!(!tool_card_spins("read", &partial_task));

        let progress = ToolExecutionRenderState {
            is_partial: true,
            result: result(json!({ "progress": { "startedAt": 1.0 } }), false),
            ..card()
        };
        assert!(tool_card_spins("bash", &progress));
        assert!(!tool_card_spins("bash", &card()));
    }

    #[test]
    fn only_a_finished_successful_todo_card_with_completed_tasks_strikes() {
        let done = ToolExecutionRenderState {
            result: result(json!({ "completedTasks": ["a"] }), false),
            ..card()
        };
        assert!(tool_card_strikes("todo", &done));
        assert!(!tool_card_strikes("bash", &done));
        let failed = ToolExecutionRenderState {
            result: result(json!({ "completedTasks": ["a"] }), true),
            ..card()
        };
        assert!(!tool_card_strikes("todo", &failed));
        let none = ToolExecutionRenderState {
            result: result(json!({ "completedTasks": [] }), false),
            ..card()
        };
        assert!(!tool_card_strikes("todo", &none));
        let partial = ToolExecutionRenderState {
            is_partial: true,
            ..done
        };
        assert!(!tool_card_strikes("todo", &partial));
    }

    #[test]
    fn the_spinner_cycles_ten_frames_and_clears_its_frame_on_stop() {
        let mut host = Host::default();
        let mut animation = ToolExecutionAnimation::new();
        animation.spin(true, 0, &mut host);
        assert!(animation.running());
        assert_eq!(animation.frame(), None);

        animation.tick(79, &mut host);
        assert_eq!(animation.frame(), None);
        animation.tick(80, &mut host);
        assert_eq!(animation.frame(), Some(0));
        animation.tick(80 * 11, &mut host);
        assert_eq!(animation.frame(), Some(0), "wraps after frame 9");
        assert_eq!(host.redraws, 11);

        animation.spin(false, 900, &mut host);
        assert!(!animation.running());
        assert_eq!(animation.frame(), None);
        assert_eq!(host.invalidations, 12);
    }

    #[test]
    fn the_strike_runs_once_through_its_frames_then_stops_and_redraws() {
        let mut host = Host::default();
        let mut animation = ToolExecutionAnimation::new();
        animation.strike(true, 0, &mut host);
        assert_eq!(animation.frame(), Some(0));
        animation.strike(true, 10, &mut host);

        let interval = TODO_STRIKE_FRAME_INTERVAL_MS;
        animation.tick(interval * TODO_STRIKE_TOTAL_FRAMES as u64, &mut host);
        assert_eq!(animation.frame(), Some(TODO_STRIKE_TOTAL_FRAMES));
        assert!(animation.running());

        animation.tick(interval * (TODO_STRIKE_TOTAL_FRAMES as u64 + 1), &mut host);
        assert!(!animation.running());
        assert_eq!(animation.frame(), None);
        assert_eq!(host.redraws, TODO_STRIKE_TOTAL_FRAMES as usize + 1);
    }

    #[test]
    fn stopping_the_spinner_keeps_the_strike_frame() {
        let mut host = Host::default();
        let mut animation = ToolExecutionAnimation::new();
        animation.spin(true, 0, &mut host);
        animation.strike(true, 0, &mut host);
        animation.spin(false, 1, &mut host);
        assert_eq!(animation.frame(), Some(0));
        animation.stop(&mut host);
        assert_eq!(animation.frame(), None);
        assert!(!animation.running());
    }
}
