#![allow(clippy::unwrap_used)]
#[path = "support.rs"]
mod support;

use maho_ext_api::{ExtensionMode, NotificationType};
use std::sync::Arc;
use std::time::Duration;

fn timeout() -> Duration {
    Duration::from_secs(5)
}

#[tokio::test]
async fn answers_without_polluting_session_history() {
    let registry = Arc::new(support::BtwRegistry::new());
    let ui = Arc::new(support::BtwUi::default());
    let ctx = support::context(ExtensionMode::Print, false, registry.clone(), ui.clone());
    let handler = support::command_handler();

    handler("what did I just ask?", &ctx).await.expect("handler");

    assert_eq!(registry.call_count(), 1);
    let (context, _options) = registry.calls.lock().unwrap_or_else(std::sync::PoisonError::into_inner).first().cloned().expect("call");
    assert_eq!(context.tools, Some(Vec::new()));
    assert_eq!(serde_json::to_value(context.messages.last().expect("question")).expect("json")["content"], serde_json::json!("what did I just ask?"));
    assert!(ctx.session_manager.get_entries().is_empty());
    assert_eq!(ui.notifications.lock().unwrap_or_else(std::sync::PoisonError::into_inner).last().expect("reply").0, "side reply");
}

#[tokio::test]
async fn empty_question_shows_usage_without_calling_the_provider() {
    let registry = Arc::new(support::BtwRegistry::new());
    let ui = Arc::new(support::BtwUi::default());
    let ctx = support::context(ExtensionMode::Print, false, registry.clone(), ui.clone());
    let handler = support::command_handler();

    handler("   ", &ctx).await.expect("handler");

    assert_eq!(registry.call_count(), 0);
    let notifications = ui.notifications.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    assert_eq!(notifications.len(), 1);
    assert_eq!(notifications[0].1, NotificationType::Warning);
    assert!(notifications[0].0.contains("/btw"));
}

#[tokio::test]
async fn a_new_btw_aborts_the_previous_side_query() {
    let registry = Arc::new(support::BtwRegistry::blocked());
    let mut observed = registry.entered.subscribe();
    let ui = Arc::new(support::BtwUi::default());
    let ctx = support::context(ExtensionMode::Print, false, registry.clone(), ui.clone());
    let handler = support::command_handler();

    let first = {
        let handler = handler.clone();
        let ctx = ctx.clone();
        tokio::spawn(async move { handler("first", &ctx).await })
    };
    assert!(matches!(tokio::time::timeout(timeout(), observed.changed()).await, Ok(Ok(()))), "stream did not start");
    let signal = observed.borrow().clone().expect("signal");

    handler("second", &ctx).await.expect("second handler");
    let completed = tokio::time::timeout(timeout(), first).await;
    assert!(completed.is_ok(), "first handler did not settle");
    assert!(signal.aborted());
}

#[tokio::test]
async fn bare_btw_dismisses_the_active_panel() {
    let registry = Arc::new(support::BtwRegistry::new());
    let ui = Arc::new(support::BtwUi::default());
    let ctx = support::context(ExtensionMode::Tui, true, registry.clone(), ui.clone());
    let handler = support::command_handler();

    handler("open the panel", &ctx).await.expect("open");
    assert_eq!(ui.widgets.lock().unwrap_or_else(std::sync::PoisonError::into_inner).len(), 1);

    handler("", &ctx).await.expect("dismiss");

    assert!(ui.widget_cleared());
    assert!(ui.notifications.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_empty());
    assert_eq!(ui.input_count(), 0);
    assert_eq!(registry.call_count(), 1);
}

#[tokio::test]
async fn bare_btw_keeps_the_usage_hint_without_a_panel() {
    let registry = Arc::new(support::BtwRegistry::new());
    let ui = Arc::new(support::BtwUi::default());
    let ctx = support::context(ExtensionMode::Tui, true, registry.clone(), ui.clone());
    let handler = support::command_handler();

    handler("", &ctx).await.expect("bare");

    assert!(ui.widgets.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_empty());
    let notifications = ui.notifications.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    assert_eq!(notifications.len(), 1);
    assert_eq!(notifications[0].1, NotificationType::Warning);
    assert_eq!(registry.call_count(), 0);
}

#[tokio::test]
async fn escape_dismisses_a_settled_panel() {
    for escape in ["\u{1b}", "\u{1b}[27u"] {
        let registry = Arc::new(support::BtwRegistry::new());
        let ui = Arc::new(support::BtwUi::default());
        let ctx = support::context(ExtensionMode::Tui, true, registry, ui.clone());
        let handler = support::command_handler();

        handler("settled question", &ctx).await.expect("open");
        assert_eq!(ui.input_count(), 1);

        ui.feed_input(escape);

        assert!(ui.widget_cleared(), "escape {escape:?}");
        assert_eq!(ui.input_count(), 0);
    }
}

#[tokio::test]
async fn a_kitty_escape_key_release_is_ignored() {
    let registry = Arc::new(support::BtwRegistry::new());
    let ui = Arc::new(support::BtwUi::default());
    let ctx = support::context(ExtensionMode::Tui, true, registry, ui.clone());
    let handler = support::command_handler();

    handler("settled question", &ctx).await.expect("open");
    assert_eq!(ui.input_count(), 1);

    ui.feed_input("\u{1b}[27;1:3u");

    assert_eq!(ui.widgets.lock().unwrap_or_else(std::sync::PoisonError::into_inner).len(), 1);
    assert_eq!(ui.input_count(), 1);
}
