use maho_ext_ask_user::{schema::{to_canonical,AskUserVariant},pending::PendingQuestion};
use maho_ext_api::{QuestionAnswer,QuestionStatus};
use serde_json::json;
use std::collections::BTreeMap;
fn pending()->PendingQuestion{PendingQuestion::new(to_canonical(AskUserVariant::Claude,&json!({"waitForAnswer":true,"questions":[{"header":"Auth","question":"Pick?","multiSelect":false}]}),"r".into(),None).expect("valid request"),100,30,Some(80))}
#[test]
fn progress_extends_idle_not_hard_cap(){let mut pending=pending();assert_eq!(pending.deadline_at_ms,130);pending.touch(120,None);assert_eq!(pending.deadline_at_ms,150);pending.touch(170,None);assert_eq!(pending.deadline_at_ms,180);assert_eq!(pending.remaining_ms(190),0);}
#[test]
fn timeout_preserves_draft_and_settles_once(){let mut pending=pending();let answers=BTreeMap::from([("q1".into(),QuestionAnswer{selected:vec!["A".into()],text:None})]);pending.touch(110,Some((answers.clone(),Some(" hi ".into()))));let result=pending.timeout(140);assert_eq!(result.answers,answers);assert_eq!(result.auto_resolved_after_ms,Some(40));assert!(result.unanswered.is_empty());assert_eq!(pending.cancel(QuestionStatus::Cancelled),result);pending.touch(200,None);assert_eq!(pending.deadline_at_ms,140);}
#[test]
fn empty_submission_stays_pending_but_partial_answer_settles(){let mut pending=pending();assert!(pending.submit(BTreeMap::new(),None).is_none());let response=pending.submit(BTreeMap::from([("other".into(),QuestionAnswer::default())]),None).unwrap();assert_eq!(response.status,QuestionStatus::Answered);assert_eq!(response.unanswered,["q1"]);}
#[test]
fn comment_submits_without_answers(){let mut pending=pending();assert_eq!(pending.submit(BTreeMap::new(),Some(" ship ".into())).unwrap().status,QuestionStatus::CommentSubmitted);}
