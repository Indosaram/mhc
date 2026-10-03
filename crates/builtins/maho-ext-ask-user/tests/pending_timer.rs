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
    timer.settle().await;
    assert!(received.changed().await.is_err());
    assert!(received.borrow().is_none());
    drop(timer);
}

#[tokio::test(start_paused = true)]
async fn ui_owned_empty_submission_retires_timer_without_timeout_delivery() {
    let (outcome, mut received) = tokio::sync::watch::channel(None);
    let timer = PendingTimer::new(request(), Arc::new(move |response| { outcome.send_replace(Some(response)); }));
    let accepted = timer.submit(BTreeMap::new(), None);
    timer.cancel(QuestionStatus::Cancelled);
    tokio::time::timeout(Duration::from_secs(1), timer.settle()).await.expect("timer retirement");
    assert!(accepted.is_none());
    assert!(received.changed().await.is_err());
    assert!(received.borrow().is_none());
}

#[tokio::test(start_paused = true)]
async fn attachment_snapshot_preserves_draft_without_resetting_hard_cap() {
    let timer=PendingTimer::new(request(),Arc::new(|_|panic!("test retires before deadline")));
    let answers=BTreeMap::from([("q1".into(),QuestionAnswer{selected:vec!["A".into()],text:None})]);
    let hard=timer.hard_deadline_at_ms();
    timer.progress(maho_ext_api::QuestionDraft{answers:Some(answers.clone()),comment:Some("draft".into())});
    let draft=timer.initial_draft();let deadline=timer.deadline_at_ms();let remaining=timer.remaining_ms();
    timer.cancel(QuestionStatus::Cancelled);timer.settle().await;
    assert_eq!(draft.answers,Some(answers));assert_eq!(draft.comment.as_deref(),Some("draft"));
    assert_eq!(timer.hard_deadline_at_ms(),hard);assert!(deadline<=hard);assert_eq!(remaining,deadline);
}

#[tokio::test(start_paused = true)]
async fn comment_progress_without_answers_preserves_selected_draft(){
    let (outcome,mut received)=tokio::sync::watch::channel(None);
    let timer=PendingTimer::new(request(),Arc::new(move|response|{outcome.send_replace(Some(response));}));
    let answers=BTreeMap::from([("q1".into(),QuestionAnswer{selected:vec!["A".into()],text:None})]);
    timer.progress(maho_ext_api::QuestionDraft{answers:Some(answers.clone()),comment:None});
    timer.progress(maho_ext_api::QuestionDraft{answers:None,comment:Some("draft".into())});
    tokio::time::advance(Duration::from_millis(100)).await;
    received.changed().await.expect("timeout event");
    let response=received.borrow().clone().expect("response");
    assert_eq!(response.answers,answers);
    assert_eq!(response.comment.as_deref(),Some("draft"));
    assert_eq!(response.status,QuestionStatus::TimedOut);
}
