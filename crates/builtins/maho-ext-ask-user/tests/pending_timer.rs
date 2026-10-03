use maho_ext_api::{QuestionAnswer, QuestionStatus};
use maho_ext_ask_user::{pending::PendingTimer, schema::{AskUserVariant, to_canonical}};
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc, time::Duration};

fn request() -> maho_ext_api::QuestionRequest {
    to_canonical(AskUserVariant::Claude, &json!({"waitForAnswer":true,"questions":[{"header":"Choice","question":"Which?","multiSelect":false}]}), "timer".into(), Some(100)).expect("request")
}
#[tokio::test(start_paused = true)]
async fn authoritative_deadline_preserves_draft_and_fires_once() {
    let (outcome, mut received) = tokio::sync::watch::channel(None);
    let timer = PendingTimer::new(request(), Arc::new(move |response| { outcome.send_replace(Some(response)); }));
    tokio::time::advance(Duration::from_millis(90)).await;
    let answers = BTreeMap::from([("q1".into(), QuestionAnswer { selected: vec!["A".into()], text: None })]);
    timer.touch(Some((answers.clone(), Some("draft".into()))));
    assert_eq!(timer.deadline_at_ms(), 190);
    tokio::time::advance(Duration::from_millis(100)).await;
    received.changed().await.expect("timeout event");
    let response = received.borrow().clone().expect("response");
    assert_eq!(response.status, QuestionStatus::TimedOut);
    assert_eq!(response.answers, answers);
    assert_eq!(response.auto_resolved_after_ms, Some(190));
    assert_eq!(timer.cancel(QuestionStatus::Cancelled), response);
    assert!(!received.has_changed().unwrap_or(false));
}
#[tokio::test(start_paused = true)]
async fn cancellation_disarms_callback_before_deadline() {
    let (outcome, mut received) = tokio::sync::watch::channel(None);
    let timer = PendingTimer::new(request(), Arc::new(move |response| { outcome.send_replace(Some(response)); }));
    assert_eq!(timer.cancel(QuestionStatus::Cancelled).status, QuestionStatus::Cancelled);
    drop(timer);
    assert!(received.changed().await.is_err());
    assert!(received.borrow().is_none());
}
