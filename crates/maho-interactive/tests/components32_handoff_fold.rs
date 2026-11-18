//! Port of the `AssistantMessageComponent folds the handoff block (senpi#3323)` cases of senpi
//! `packages/coding-agent/test/assistant-message-handoff-fold.test.ts`.
//!
//! The segment-splitting half of that file lives beside its implementation in
//! `crates/maho-interactive/src/components/handoff_fold.rs`; this file drives the component end to
//! end: the folded line, expand-all, the click toggle, the GPT shape, an empty `For you:` slot, and
//! an answer with no block at all.

use maho_interactive::components::assistant_message::AssistantMessageComponent;
use maho_interactive::components::markdown_transform::get_markdown_theme;
use maho_interactive::theme::{ColorMode, Theme};
use maho_tui::tui::{Component, TuiMouseButton, TuiMouseEvent, TuiMouseEventType};
use maho_tui::utils::visible_width;
use serde_json::json;

const BLOCK: &str = "Ask: fix the login redirect - wanted: users land on the dashboard. For you: the redirect now reads the saved path; 12 tests pass. Now: Post milestones and report. Next: Open the PR.";
const GPT_BLOCK: &str = "Redirect fixed and the login works for every user. You need: 3/3 done, no blockers. Now: Report the fix. Next: none.";

fn theme() -> Theme {
    Theme::builtin("dark", ColorMode::Color256).expect("builtin theme")
}

fn message(text: &str) -> serde_json::Value {
    json!({
        "role": "assistant",
        "content": [{ "type": "text", "text": text }],
        "api": "openai-responses",
        "provider": "openai",
        "model": "gpt-4o-mini",
        "usage": {
            "input": 0,
            "output": 0,
            "cacheRead": 0,
            "cacheWrite": 0,
            "totalTokens": 0,
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 }
        },
        "stopReason": "stop",
        "timestamp": 0
    })
}

fn component(text: &str, theme: &Theme) -> AssistantMessageComponent {
    AssistantMessageComponent::new(
        Some(message(text)),
        false,
        get_markdown_theme(theme),
        "Thinking...",
        1,
        Vec::new(),
        theme.clone(),
    )
}

fn plain(component: &mut AssistantMessageComponent, width: usize) -> Vec<String> {
    component
        .render(width)
        .into_iter()
        .map(|line| maho_tui::utils::strip_terminal_sequences(&line))
        .collect()
}

/// Each child before the fold line renders exactly one row at the widths these cases use, so a
/// rendered row index doubles as a child index.
fn click(component: &mut AssistantMessageComponent, row: usize, width: usize) -> bool {
    let event = TuiMouseEvent {
        event_type: TuiMouseEventType::Click,
        button: TuiMouseButton::Left,
        x: 1,
        y: 0,
        screen_x: 1,
        screen_y: row as i64,
        width,
        height: 1,
        shift: false,
        alt: false,
        ctrl: false,
        wheel_delta: None,
        click_count: None,
    };
    let child = component
        .as_container()
        .expect("assistant message container")
        .children[row]
        .clone();
    let result = child.borrow_mut().handle_mouse(&event);
    result.is_some_and(|result| result.handled)
}

fn fold_row(component: &mut AssistantMessageComponent, width: usize) -> usize {
    plain(component, width)
        .iter()
        .position(|line| line.contains("Handoff"))
        .expect("a fold line")
}

#[test]
fn collapsed_shows_one_muted_line_with_now_and_next_then_the_answer() {
    let theme = theme();
    let mut component = component(BLOCK, &theme);
    let lines = plain(&mut component, 60);
    let fold = lines
        .iter()
        .find(|line| line.contains("\u{25B8} Handoff"))
        .expect("a collapsed fold line")
        .clone();
    assert!(fold.contains("Now: Post milestones"), "{fold:?}");
    assert!(visible_width(&fold) <= 60, "{fold:?}");
    let rendered = lines.join("\n");
    assert!(rendered.contains("the redirect now reads the saved path"));
    assert!(!rendered.contains("Ask:"));
    assert!(!rendered.contains("For you:"));
}

#[test]
fn expand_all_shows_the_block_as_written() {
    let theme = theme();
    let mut component = component(BLOCK, &theme);
    component.set_expanded(true);
    let rendered = plain(&mut component, 80).join("\n");
    assert!(rendered.contains("\u{25BE} Handoff"));
    let collapsed = rendered.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        collapsed.contains("Ask: fix the login redirect - wanted:"),
        "{collapsed}"
    );
}

#[test]
fn a_click_on_the_fold_line_opens_that_block_and_a_second_click_closes_it() {
    let theme = theme();
    let width = 80;
    let mut component = component(BLOCK, &theme);
    let row = fold_row(&mut component, width);
    assert!(click(&mut component, row, width), "first click handled");
    assert!(plain(&mut component, width).join("\n").contains("Ask:"));
    let row = fold_row(&mut component, width);
    assert!(click(&mut component, row, width), "second click handled");
    assert!(!plain(&mut component, width).join("\n").contains("Ask:"));
}

#[test]
fn the_gpt_block_folds_with_its_outcome_shown_as_the_answer() {
    let theme = theme();
    let mut component = component(GPT_BLOCK, &theme);
    let rendered = plain(&mut component, 80).join("\n");
    assert!(rendered.contains("\u{25B8} Handoff"));
    assert!(rendered.contains("Redirect fixed and the login works for every user."));
    assert!(!rendered.contains("You need:"));
}

#[test]
fn a_block_with_an_empty_for_you_folds_to_the_line_alone() {
    let theme = theme();
    let mut component = component(
        "Ask: check - wanted: a check. For you: Now: none. Next: none.",
        &theme,
    );
    let non_empty = plain(&mut component, 80)
        .into_iter()
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>();
    assert_eq!(non_empty.len(), 1, "{non_empty:?}");
    assert!(non_empty[0].contains("\u{25B8} Handoff"), "{non_empty:?}");
}

#[test]
fn an_answer_with_no_block_renders_unchanged() {
    let theme = theme();
    let text = "The function parses the header and returns its length.";
    let mut component = component(text, &theme);
    let rendered = plain(&mut component, 80).join("\n");
    assert!(rendered.contains(text));
    assert!(!rendered.contains("Handoff"));
}
