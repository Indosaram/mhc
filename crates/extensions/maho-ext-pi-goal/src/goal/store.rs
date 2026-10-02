use std::{io::Write,path::PathBuf};
use super::{types::*,validation::{validate_objective,resolve_token_budget,js_whitespace},transitions::transition_goal_status};
fn encoded_thread_id(reference:&GoalStoreRef)->String{let mut result=String::new();for byte in reference.thread_id.bytes(){if byte.is_ascii_alphanumeric()||b"-_.!~*'()".contains(&byte){result.push(char::from(byte));}else{result.push_str(&format!("%{byte:02X}"));}}result}
pub fn goal_file_path(reference:&GoalStoreRef)->PathBuf{reference.base_dir.join(format!("{}.json",encoded_thread_id(reference)))}
pub fn goal_history_file_path(reference:&GoalStoreRef)->PathBuf{reference.base_dir.join(format!("{}.history.jsonl",encoded_thread_id(reference)))}
pub fn objective_full_text_file_name(reference:&GoalStoreRef)->String{format!("{}.objective-full.txt",encoded_thread_id(reference))}
pub fn objective_full_text_file_path(reference:&GoalStoreRef)->PathBuf{reference.base_dir.join(objective_full_text_file_name(reference))}
pub fn read_goal(reference:&GoalStoreRef)->Result<Option<Goal>,String>{match std::fs::read_to_string(goal_file_path(reference)){Ok(raw)=>parse_goal_file(&raw),Err(error)if error.kind()==std::io::ErrorKind::NotFound=>Ok(None),Err(error)=>Err(error.to_string())}}
pub fn write_goal(reference:&GoalStoreRef,goal:Option<&Goal>)->Result<(),String>{std::fs::create_dir_all(&reference.base_dir).map_err(|e|e.to_string())?;let text=serde_json::to_string_pretty(&serde_json::json!({"version":1,"goal":goal})).map_err(|e|e.to_string())?;std::fs::write(goal_file_path(reference),format!("{text}\n")).map_err(|e|e.to_string())}
pub fn parse_goal_file(raw:&str)->Result<Option<Goal>,String>{
    let value:serde_json::Value=serde_json::from_str(raw).map_err(|e|e.to_string())?;
    let object=value.as_object().ok_or("goal store must be a JSON object")?;
    if object.get("version").and_then(serde_json::Value::as_f64)!=Some(1.0){return Err("unsupported goal store version".into());}
    let invalid=||"goal store contains an invalid goal".to_owned();let goal=object.get("goal").ok_or_else(invalid)?;
    if goal.is_null(){return Ok(None);}
    let map=goal.as_object().ok_or_else(invalid)?;
    let safe=|value:&serde_json::Value|value.as_f64().is_some_and(|number|number.is_finite()&&(0.0..=9_007_199_254_740_991.0).contains(&number)&&number.fract()==0.0);
    for key in ["tokensUsed","timeUsedSeconds","createdAt","updatedAt"]{if !map.get(key).is_some_and(safe){return Err(invalid());}}
    for key in ["tokenBudget","lastStartedAt","completedAt"]{if map.get(key).is_some_and(|value|!safe(value)){return Err(invalid());}}
    let mut normalized=goal.clone();
    for key in ["tokensUsed","timeUsedSeconds","createdAt","updatedAt","tokenBudget","lastStartedAt","completedAt","blockedAt"]{if let Some(number)=map.get(key).and_then(serde_json::Value::as_f64)&&safe(&map[key]){normalized[key]=serde_json::from_str(&format!("{number:.0}")).map_err(|_|invalid())?;}}
    let parsed:Goal=serde_json::from_value(normalized).map_err(|_|invalid())?;
    if parsed.status==GoalStatus::Blocked{if !map.get("blockedAt").is_some_and(safe)||parsed.blocked_reason.as_ref().is_none_or(|reason|reason.trim_matches(js_whitespace).is_empty()){return Err(invalid());}}else if map.contains_key("blockedReason")||map.contains_key("blockedAt"){return Err(invalid());}
    Ok(Some(parsed))
}
pub fn archive_goal(reference:&GoalStoreRef,goal:&Goal)->Result<(),String>{std::fs::create_dir_all(&reference.base_dir).map_err(|e|e.to_string())?;let mut file=std::fs::OpenOptions::new().create(true).append(true).open(goal_history_file_path(reference)).map_err(|e|e.to_string())?;writeln!(file,"{}",serde_json::to_string(goal).map_err(|e|e.to_string())?).map_err(|e|e.to_string())}
pub fn clear_goal(reference:&GoalStoreRef)->Result<bool,String>{let had=read_goal(reference)?.is_some();write_goal(reference,None)?;Ok(had)}
fn write_full(reference:&GoalStoreRef,objective:&str)->Result<(),String>{std::fs::create_dir_all(&reference.base_dir).map_err(|e|e.to_string())?;std::fs::write(objective_full_text_file_path(reference),objective).map_err(|e|e.to_string())}
fn now_seconds()->u64{std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs()}
pub fn create_goal(reference:&GoalStoreRef,objective:&str)->Result<Goal,String>{create_goal_at(reference,objective,now_seconds(),uuid::Uuid::new_v4().to_string())}
pub fn create_goal_at(reference:&GoalStoreRef,objective:&str,now:u64,id:String)->Result<Goal,String>{
    let validated=validate_objective(objective,&objective_full_text_file_name(reference))?;let current=read_goal(reference)?;
    if current.as_ref().is_some_and(|goal|goal.status!=GoalStatus::Complete){return Err("cannot create a new goal because this thread already has a goal".into());}
    if validated.truncated{write_full(reference,objective)?;}
    if let Some(current)=current{archive_goal(reference,&current)?;}
    let goal=Goal{id,thread_id:reference.thread_id.clone(),objective:validated.objective,status:GoalStatus::Active,token_budget:None,tokens_used:0,time_used_seconds:0,created_at:now,updated_at:now,last_started_at:Some(now),blocked_reason:None,blocked_at:None,completed_at:None};write_goal(reference,Some(&goal))?;Ok(goal)
}
pub fn update_goal(reference:&GoalStoreRef,update:&GoalUpdate,source:GoalUpdateSource)->Result<Goal,String>{update_goal_at(reference,update,source,now_seconds(),uuid::Uuid::new_v4().to_string())}
pub fn account_goal_usage(reference:&GoalStoreRef,usage:&TokenUsageSnapshot,elapsed:f64,mode:GoalAccountingMode,expected_id:Option<&str>)->Result<Option<Goal>,String>{account_goal_usage_at(reference,usage,elapsed,mode,expected_id,now_seconds())}
pub fn account_goal_usage_at(reference:&GoalStoreRef,usage:&TokenUsageSnapshot,elapsed:f64,mode:GoalAccountingMode,expected_id:Option<&str>,now:u64)->Result<Option<Goal>,String>{
    let Some(mut goal)=read_goal(reference)?else{return Ok(None);};
    let allowed=match mode{GoalAccountingMode::Active=>goal.status==GoalStatus::Active,GoalAccountingMode::ActiveOrBlocked=>matches!(goal.status,GoalStatus::Active|GoalStatus::Blocked),GoalAccountingMode::ActiveOrComplete=>matches!(goal.status,GoalStatus::Active|GoalStatus::Complete)};
    if expected_id.is_some_and(|id|goal.id!=id)||!allowed{return Ok(Some(goal));}
    let tokens=usage.input.max(0.0)+usage.output.max(0.0);
    goal.tokens_used+=format!("{tokens:.0}").parse::<u64>().map_err(|e|e.to_string())?;
    goal.time_used_seconds+=format!("{:.0}",elapsed.trunc().max(0.0)).parse::<u64>().map_err(|e|e.to_string())?;
    goal.updated_at=now.max(goal.updated_at+1);write_goal(reference,Some(&goal))?;Ok(Some(goal))
}
pub fn update_goal_at(reference:&GoalStoreRef,update:&GoalUpdate,source:GoalUpdateSource,now:u64,id:String)->Result<Goal,String>{
    let current=read_goal(reference)?.ok_or("cannot update goal: no goal exists")?;let validated=update.objective.as_ref().map(|objective|validate_objective(objective,&objective_full_text_file_name(reference))).transpose()?;
    let objective=validated.as_ref().map_or(&current.objective,|v|&v.objective).clone();let token_budget=resolve_token_budget(current.token_budget,update.token_budget)?;let now=now.max(current.updated_at+1);let requested=update.status.or(update.objective.as_ref().map(|_|GoalStatus::Active));
    let next=if update.objective.is_some()&&(objective!=current.objective||current.status==GoalStatus::Complete){let status=requested.unwrap_or(GoalStatus::Active);if status==GoalStatus::Blocked{return Err("objective replacement cannot create a blocked goal".into());}Goal{id,thread_id:reference.thread_id.clone(),objective,status,token_budget,tokens_used:0,time_used_seconds:0,created_at:now,updated_at:now,last_started_at:(status==GoalStatus::Active).then_some(now),blocked_reason:None,blocked_at:None,completed_at:(status==GoalStatus::Complete).then_some(now)}}else{let mut next=transition_goal_status(&Goal{objective,..current.clone()},requested.unwrap_or(current.status),source,update.reason.as_deref(),now)?;next.token_budget=token_budget;next};
    if validated.is_some_and(|v|v.truncated){write_full(reference,update.objective.as_deref().unwrap_or(""))?;}write_goal(reference,Some(&next))?;Ok(next)
}
