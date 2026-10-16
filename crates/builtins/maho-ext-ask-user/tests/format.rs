use maho_ext_ask_user::{schema::AskUserVariant,format::{parse_ask_user_answer_frame,format_result_details,format_required_no_answer}};
use maho_ext_api::{Question,QuestionAnswer,QuestionResponse,QuestionStatus};
use std::collections::BTreeMap;
use serde_json::json;
#[test]
fn frame_parser_requires_sentinel_and_line_boundary(){assert_eq!(parse_ask_user_answer_frame("[Answer to question r]\r\nbody\nmore"),Some(("r","body\nmore")));for text in ["[Answer to question ]\nbody","[Answer to question r]body","prefix[Answer to question r]\nbody","[Answer to question r\ns]\nbody"]{assert!(parse_ask_user_answer_frame(text).is_none());}}
#[test]
fn variant_machine_details(){let response=QuestionResponse{status:QuestionStatus::Answered,answers:BTreeMap::from([("q1".into(),QuestionAnswer{selected:vec!["A".into(),"B".into()],text:Some("ignored".into())}),("q2".into(),QuestionAnswer{selected:vec![],text:Some(" free ".into())})]),comment:Some("comment".into()),unanswered:vec!["q3".into()],auto_resolved_after_ms:None};let questions=vec![Question{id:"q1".into(),header:"Pick".into(),question:"Which?".into(),options:vec![],multi_select:true}];let codex=format_result_details(AskUserVariant::Codex,&response,&questions);assert_eq!(codex["answers"]["q1"],json!({"answers":["A","B"]}));assert_eq!(codex["answers"]["q2"],json!({"answers":["free"]}));let claude=format_result_details(AskUserVariant::Claude,&response,&questions);assert_eq!(claude["answers"]["Which?"],"A, B");assert_eq!(claude["freeText"],"comment");assert_eq!(claude["questions"][0]["multiSelect"],true);}
#[test]
fn required_no_answer_refuses_gated_action_and_marks_drafts_unanswered(){
    let response=QuestionResponse{status:QuestionStatus::TimedOut,answers:BTreeMap::from([("q1".into(),QuestionAnswer{selected:vec!["Proceed".into()],text:None})]),comment:None,unanswered:vec!["q2".into()],auto_resolved_after_ms:Some(60_000)};
    let questions=vec![Question{id:"q1".into(),header:"Action".into(),question:"Proceed?".into(),options:vec![],multi_select:false},Question{id:"q2".into(),header:"Reason".into(),question:"Why?".into(),options:vec![],multi_select:false}];
    let text=format_required_no_answer(&response,&questions);
    assert!(text.contains("Before going idle the user had selected (not an answer): Action: Proceed"));
    assert!(text.ends_with("No answer: do not take the action it gates. Keep that action pending and end the turn."));
}
