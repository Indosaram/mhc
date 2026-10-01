use std::cell::RefCell;
use std::rc::Rc;

use maho_interactive::components::progressive_transcript_container::{
    ProgressiveTranscriptContainer, ProgressiveTranscriptOptions,
};
use maho_interactive::components::tool_renderer_boundary::ToolRendererBoundary;
use maho_tui::components::text::Text;
use maho_tui::tui::{Component, TuiMouseEvent, TuiMouseEventResult};
use serde_json::Value;

struct Throwing;

impl Component for Throwing {
    fn render(&mut self, _width: usize) -> Vec<String> {
        panic!("boom");
    }
    fn invalidate(&mut self) {}
}

struct Counting {
    renders: Rc<RefCell<usize>>,
}

impl Component for Counting {
    fn render(&mut self, _width: usize) -> Vec<String> {
        *self.renders.borrow_mut() += 1;
        vec![String::from("healthy")]
    }
    fn invalidate(&mut self) {}
    fn handle_mouse(&mut self, _event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        None
    }
}

fn fixtures() -> Value {
    serde_json::from_str(include_str!("golden/components32-boundary.json")).expect("pinned fixture")
}

fn expected(case: &Value) -> Vec<String> {
    case["lines"].as_array().expect("lines").iter().map(|line| line.as_str().expect("line").to_owned()).collect()
}

fn trim(lines: Vec<String>) -> Vec<String> {
    lines.into_iter().map(|line| line.trim_end().to_owned()).collect()
}

#[test]
fn tool_renderer_boundary_matches_pinned_senpi_failure_and_recovery_paths() {
    for case in fixtures()["boundary"].as_array().expect("boundary") {
        let width = case["width"].as_u64().expect("width") as usize;
        let name = case["name"].as_str().expect("name");
        let failures = Rc::new(RefCell::new(0usize));
        let counter = Rc::clone(&failures);
        let mut boundary = if name == "healthy" {
            ToolRendererBoundary::new(
                Rc::new(RefCell::new(Counting { renders: Rc::new(RefCell::new(0)) })),
                None,
                Box::new(move || *counter.borrow_mut() += 1),
            )
        } else {
            ToolRendererBoundary::new(
                Rc::new(RefCell::new(Throwing)),
                Some(Rc::new(RefCell::new(Text::with_padding("fallback", 0, 0)))),
                Box::new(move || *counter.borrow_mut() += 1),
            )
        };
        let first = trim(boundary.render(width));
        assert_eq!(first, expected(case), "boundary {name} at {width}");
        assert_eq!(*failures.borrow(), case["failures"].as_u64().expect("failures") as usize, "boundary {name} failures");
    }
}

#[test]
fn tool_renderer_boundary_keeps_failing_after_the_first_failure() {
    let failures = Rc::new(RefCell::new(0usize));
    let counter = Rc::clone(&failures);
    let mut boundary = ToolRendererBoundary::new(
        Rc::new(RefCell::new(Throwing)),
        Some(Rc::new(RefCell::new(Text::with_padding("fallback", 0, 0)))),
        Box::new(move || *counter.borrow_mut() += 1),
    );
    let first = trim(boundary.render(40));
    let second = trim(boundary.render(40));
    assert_eq!(first, second);
    assert_eq!(*failures.borrow(), 1, "the failure callback must fire once");
}

#[test]
fn progressive_transcript_paints_only_the_tail_and_hydrates_on_demand() {
    for case in fixtures()["progressive"].as_array().expect("progressive") {
        let count = case["count"].as_u64().expect("count") as usize;
        let mut container = ProgressiveTranscriptContainer::new(ProgressiveTranscriptOptions {
            tail_budget: case["tailBudget"].as_u64().expect("tailBudget") as usize,
            warm_chunk_size: case["warmChunkSize"].as_u64().expect("warmChunkSize") as usize,
            request_render: Rc::new(|| {}),
        });
        for index in 0..count {
            container.add_child(Rc::new(RefCell::new(Text::with_padding(format!("child-{index}"), 0, 0))));
        }
        for (frame, wanted) in case["frames"].as_array().expect("frames").iter().enumerate() {
            let expected_lines: Vec<String> =
                wanted.as_array().expect("frame").iter().map(|line| line.as_str().expect("line").to_owned()).collect();
            assert_eq!(trim(container.render(40)), expected_lines, "progressive count={count} frame={frame}");
        }
    }
}

#[test]
fn progressive_transcript_reaches_the_full_history_after_enough_hydration_steps() {
    let mut container = ProgressiveTranscriptContainer::new(ProgressiveTranscriptOptions {
        tail_budget: 2,
        warm_chunk_size: 1,
        request_render: Rc::new(|| {}),
    });
    for index in 0..5 {
        container.add_child(Rc::new(RefCell::new(Text::with_padding(format!("child-{index}"), 0, 0))));
    }
    assert_eq!(trim(container.render(40)), vec!["child-3", "child-4"]);
    assert!(!container.is_fully_hydrated());
    for _ in 0..4 {
        container.warm_next_chunk();
    }
    assert!(container.is_fully_hydrated());
    assert_eq!(
        trim(container.render(40)),
        vec!["child-0", "child-1", "child-2", "child-3", "child-4"]
    );
}
