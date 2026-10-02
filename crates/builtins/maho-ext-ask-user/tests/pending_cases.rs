use maho_ext_ask_user::{pending::PendingQuestion, schema::{to_canonical,AskUserVariant}};
use maho_ext_api::{QuestionAnswer,QuestionStatus};
use serde_json::json;
use std::collections::BTreeMap;

const MINUTE: u64 = 60_000;
fn pending() -> PendingQuestion {
    let request = to_canonical(AskUserVariant::Claude,&json!({"waitForAnswer":true,"questions":[{"header":"Approach","question":"Which?","multiSelect":false},{"header":"Library","question":"Which?","multiSelect":false}]}),"req-1".into(),Some(30*MINUTE)).expect("valid request");
    PendingQuestion::new(request,0,30*MINUTE,None)
}
fn answers() -> BTreeMap<String,QuestionAnswer> {
    BTreeMap::from([("q1".into(),QuestionAnswer { selected:vec!["A".into()],text:None }),("q2".into(),QuestionAnswer { selected:vec!["C".into()],text:None })])
}
#[test]
fn idle_boundary_and_elapsed_metadata() {
    let mut pending=pending();
    assert_eq!(pending.remaining_ms(30*MINUTE-1),1);
    assert!(pending.result.is_none());
    let result=pending.timeout(30*MINUTE);
    assert_eq!(result.status,QuestionStatus::TimedOut);
    assert_eq!(result.auto_resolved_after_ms,Some(30*MINUTE));
}
#[test]
fn touch_at_twenty_nine_minutes_moves_deadline_to_fifty_nine() {
    let mut pending=pending();pending.touch(29*MINUTE,None);
    assert_eq!(pending.deadline_at_ms,59*MINUTE);
    assert_eq!(pending.remaining_ms(29*MINUTE),30*MINUTE);
    assert_eq!(pending.remaining_ms(29*MINUTE),30*MINUTE);
}
#[test]
fn repeated_touches_preserve_two_hour_hard_cap() {
    let mut pending=pending();
    for minute in [29,58,87,116] { pending.touch(minute*MINUTE,None); }
    assert_eq!(pending.deadline_at_ms,120*MINUTE);
    assert_eq!(pending.timeout(120*MINUTE).auto_resolved_after_ms,Some(120*MINUTE));
}
#[test]
fn comment_only_leaves_both_questions_unanswered() {
    let result=pending().submit(BTreeMap::new(),Some("just do it".into())).expect("submitted");
    assert_eq!(result.status,QuestionStatus::CommentSubmitted);
    assert_eq!(result.unanswered,["q1","q2"]);
}
#[test]
fn full_submission_answers_all_questions() {
    let result=pending().submit(answers(),None).expect("submitted");
    assert_eq!(result.status,QuestionStatus::Answered);assert!(result.unanswered.is_empty());
}
#[test]
fn partial_submission_preserves_unanswered_ids() {
    let mut answer=answers();answer.remove("q2");
    assert_eq!(pending().submit(answer,None).expect("submitted").unanswered,["q2"]);
}
#[test]
fn empty_comment_does_not_settle_empty_answers() {
    let mut pending=pending();assert!(pending.submit(BTreeMap::new(),Some(String::new())).is_none());assert!(pending.result.is_none());
}
#[test]
fn submission_after_timeout_returns_existing_result() {
    let mut pending=pending();let result=pending.timeout(30*MINUTE);
    assert_eq!(pending.submit(answers(),Some("nope".into())),Some(result));
}
#[test]
fn draft_selections_and_comment_survive_timeout() {
    let mut pending=pending();let mut answer=answers();answer.remove("q2");
    pending.touch(0,Some((answer.clone(),Some("maybe later".into()))));
    let result=pending.timeout(30*MINUTE);
    assert_eq!(result.answers,answer);assert_eq!(result.comment.as_deref(),Some("maybe later"));assert_eq!(result.unanswered,["q2"]);
}
