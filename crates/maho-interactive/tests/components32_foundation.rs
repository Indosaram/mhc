use std::rc::Rc;
use maho_interactive::{components::{dynamic_border::DynamicBorder, continuity_notice::ContinuityNoticeTracker, markdown_transform::{create_markdown_transform, MessageType}}, theme::{Theme, ColorMode}};
use maho_tui::tui::Component;
use serde_json::Value;

fn theme() -> Theme { Theme::builtin("dark", ColorMode::Color256).expect("builtin theme") }
fn fixture() -> Value { serde_json::from_str(include_str!("golden/components32-foundation.json")).expect("pinned fixture") }
#[test]
fn borders_match_pinned_senpi_at_zero_and_regular_widths() {
    for case in fixture()["borders"].as_array().expect("borders") {
        let width = usize::try_from(case["width"].as_u64().expect("width")).expect("width fits");
        assert_eq!(serde_json::json!(DynamicBorder::new(theme()).render(width)), case["lines"]);
    }
}
#[test]
fn continuity_notices_match_all_pinned_metrics_and_diagnostics() {
    for case in fixture()["continuity"].as_array().expect("continuity") {
        assert_eq!(serde_json::json!(ContinuityNoticeTracker::default().notice_for(&case["message"], &theme())), case["notice"]);
    }
}
#[test]
fn disabled_notice_is_suppressed_until_transcript_reset() {
    let mut tracker = ContinuityNoticeTracker::default();
    let message = serde_json::json!({"diagnostics":[{"type":"claude_sdk_oauth_session_continuity","details":{"kind":"disabled"}}]});
    assert!(tracker.notice_for(&message, &theme()).is_some());
    assert!(tracker.notice_for(&message, &theme()).is_none());
    tracker.reset();
    assert!(tracker.notice_for(&message, &theme()).is_some());
}
#[test]
fn transformers_preserve_last_success_and_pass_context_in_order() {
    let transform = create_markdown_transform(MessageType::AssistantThinking, true, vec![
        Rc::new(|text, context| { assert_eq!(context.message_type, MessageType::AssistantThinking); assert!(context.is_streaming); assert_eq!(context.available_width, 42); Ok(Some(format!("{text} one"))) }),
        Rc::new(|_, _| Err("extension failed".into())),
        Rc::new(|_, _| Ok(None)),
        Rc::new(|text, _| Ok(Some(format!("{text} two")))),
    ]);
    assert_eq!(transform("start", 42), "start one two");
}
#[test]
fn user_and_branch_cards_match_pinned_output_at_multiple_widths() {
    use maho_interactive::components::{user_message::UserMessageComponent, branch_summary_message::BranchSummaryMessageComponent, markdown_transform::get_markdown_theme};
    for case in fixture()["messages"].as_array().expect("messages") {
        let width = usize::try_from(case["width"].as_u64().expect("width")).expect("width fits");
        let text = case["text"].as_str().unwrap_or_default().to_owned();
        let actual = if case["kind"] == "user" {
            UserMessageComponent::new(text, theme(), get_markdown_theme(&theme()), 1, Vec::new()).render(width)
        } else if case["kind"] == "branch" {
            let mut component = BranchSummaryMessageComponent::new(text, theme(), get_markdown_theme(&theme()), String::new());
            component.set_expanded(case["expanded"].as_bool().expect("expanded"));
            component.render(width)
        } else if case["kind"] == "compaction" {
            let mut component = maho_interactive::components::compaction_summary_message::CompactionSummaryMessageComponent::new(case["message"].clone(), theme(), get_markdown_theme(&theme()), String::new());
            component.set_expanded(case["expanded"].as_bool().expect("expanded"));
            component.render(width)
        } else {
            use maho_interactive::components::skill_invocation_message::{InvokedSkill, SkillInvocationMessageComponent};
            let skills = case["skills"].as_array().expect("skills").iter().map(|skill| InvokedSkill { name: skill["name"].as_str().expect("name").into(), content: skill["content"].as_str().expect("content").into() }).collect();
            let mut component = SkillInvocationMessageComponent::new(skills, theme(), get_markdown_theme(&theme()), String::new());
            component.set_expanded(case["expanded"].as_bool().expect("expanded"));
            component.render(width)
        };
        assert_eq!(serde_json::json!(actual), case["lines"], "{} at {width}", case["kind"]);
    }
}
