use maho_ext_api::{Question,QuestionAnswer,QuestionResponse,QuestionStatus};
use serde_json::{Value,json,Map};
use crate::schema::AskUserVariant;
fn answer_body(answer:&QuestionAnswer)->Option<String>{if !answer.selected.is_empty(){Some(answer.selected.join(", "))}else{answer.text.as_ref().map(|text|text.trim().to_owned()).filter(|text|!text.is_empty())}}
pub fn status_name(status:QuestionStatus)->&'static str{match status{QuestionStatus::Answered=>"answered",QuestionStatus::CommentSubmitted=>"comment-submitted",QuestionStatus::TimedOut=>"timed_out",QuestionStatus::Cancelled=>"cancelled",QuestionStatus::OrphanedAfterRestart=>"orphaned-after-restart",QuestionStatus::Unavailable=>"unavailable"}}
pub fn format_result_details(variant:AskUserVariant,response:&QuestionResponse,questions:&[Question])->Value{
    let mut answers=Map::new();
    for (id,answer) in &response.answers{
        match variant{
            AskUserVariant::Codex=>{let selected=if !answer.selected.is_empty(){answer.selected.clone()}else{answer_body(answer).into_iter().collect()};answers.insert(id.clone(),json!({"answers":selected}));}
            AskUserVariant::Claude=>{if let Some(body)=answer_body(answer){let key=questions.iter().find(|question|question.id==*id).map_or(id.as_str(),|question|question.question.as_str());answers.insert(key.into(),body.into());}}
        }
    }
    let mut details=json!({"answers":answers,"status":status_name(response.status),"unanswered":response.unanswered.iter().map(|id|if variant==AskUserVariant::Claude{questions.iter().find(|question|question.id==*id).map_or(id.as_str(),|question|question.question.as_str())}else{id.as_str()}).collect::<Vec<_>>()});
    if variant==AskUserVariant::Claude{details["questions"]=Value::Array(questions.iter().map(|question|json!({"id":question.id,"header":question.header,"question":question.question,"multiSelect":question.multi_select,"options":question.options.iter().map(|option|{let mut value=json!({"label":option.label});if let Some(description)=&option.description{value["description"]=description.clone().into();}value}).collect::<Vec<_>>()})).collect());}
    if let Some(comment)=&response.comment{details[if variant==AskUserVariant::Codex{"comment"}else{"freeText"}]=comment.clone().into();}details
}
pub fn parse_ask_user_answer_frame(text:&str)->Option<(&str,&str)>{
    let rest=text.strip_prefix("[Answer to question ")?;let end=rest.find(']')?;let id=&rest[..end];if id.is_empty()||id.contains(['\r','\n']){return None;}
    let rest=rest[end+1..].strip_prefix("\r\n").or_else(||rest[end+1..].strip_prefix('\n'))?;Some((id,rest))
}
fn answered_lines(response:&QuestionResponse,questions:&[Question])->Vec<String>{
    let mut lines=Vec::new();let mut seen=std::collections::HashSet::new();
    for question in questions{if let Some(body)=response.answers.get(&question.id).and_then(answer_body){lines.push(format!("{}: {body}",question.header));seen.insert(question.id.as_str());}}
    for (id,answer) in &response.answers{if !seen.contains(id.as_str())&&let Some(body)=answer_body(answer){let header=questions.iter().find(|question|question.id==*id).map_or(id.as_str(),|question|question.header.as_str());lines.push(format!("{header}: {body}"));}}
    lines
}
pub fn format_result_text(response:&QuestionResponse,questions:&[Question])->String{
    match response.status{
        QuestionStatus::Answered|QuestionStatus::CommentSubmitted=>{
            let mut lines=Vec::new();if response.status==QuestionStatus::CommentSubmitted{lines.push(format!("The user responded: {}",response.comment.as_deref().unwrap_or("").trim()));}lines.extend(answered_lines(response,questions));
            if !response.unanswered.is_empty(){lines.push(format!("Unanswered: {}",response.unanswered.iter().map(|id|questions.iter().find(|question|question.id==*id).map_or(id.as_str(),|question|question.header.as_str())).collect::<Vec<_>>().join(", ")));}lines.join("\n")
        }
        QuestionStatus::TimedOut=>{
            let minutes=(response.auto_resolved_after_ms.unwrap_or(crate::schema::DEFAULT_ASK_USER_TIMEOUT_MS) as f64/60_000.0).round();
            let mut lines=vec![format!("The user did not answer within {minutes} minutes. (사용자가 답변을 안하고 timeout 으로 종료됨)")];let selected=answered_lines(response,questions);
            if !selected.is_empty(){lines.push(format!("Before going idle the user had selected: {}",selected.join("; ")));}lines.push("Continue the work to completion on your best judgment; do not ask this question again this turn.".into());lines.join("\n")
        }
        QuestionStatus::Cancelled=>"The user dismissed the question.".into(),
        QuestionStatus::OrphanedAfterRestart=>"The pending question could not be resumed after a restart; continue on best judgment.".into(),
        QuestionStatus::Unavailable=>"This session has no user attached (subagent or headless); decide on best judgment.".into(),
    }
}
pub fn format_user_message(response:&QuestionResponse,request_id:&str,questions:&[Question])->String{format!("[Answer to question {request_id}]\n{}",format_result_text(response,questions))}
