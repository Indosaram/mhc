use std::sync::Arc;
use maho_ext_api::*;
use serde_json::json;

pub fn targets_text(result:&RelocationTargetsResult)->String {
    match result {
        RelocationTargetsResult::Listed{targets} if targets.is_empty()=>"No other computer is offered right now. Keep working on this computer.".into(),
        RelocationTargetsResult::Listed{targets}=>format!("You can continue this session on:\n{}",targets.iter().map(describe_target).collect::<Vec<_>>().join("\n")),
        RelocationTargetsResult::Unavailable{detail,..}|RelocationTargetsResult::Refused{detail,..}=>format!("Moving to another computer is not possible here ({detail}). Keep working on this computer."),
    }
}
fn describe_target(target:&RelocationTarget)->String {
    let mut notes=vec![if target.online{"online".into()}else{"offline".into()}];
    if let Some(capacity)=&target.capacity{notes.push(format!("capacity: {capacity}"));}
    if target.about_to_sleep==Some(true){notes.push("about to sleep".into());}
    format!("- {} ({})",target.name,notes.join(", "))
}
pub fn request_text(result:&RelocationRequestResult)->String {
    match result {
        RelocationRequestResult::Scheduled{target,..}=>format!("The session will continue on {target} when this turn ends. Finish this turn with a short note for the user; the conversation and any queued messages continue on {target}, so do not start new work here."),
        RelocationRequestResult::Unavailable{detail,..}=>format!("This session can't move to another computer ({detail}). It stays on this computer; keep working here."),
        RelocationRequestResult::Refused{detail,..}=>format!("The move did not happen: {detail}. The session stays on this computer; keep working here."),
    }
}
pub const RELOCATION_TOOLS:[&str;2]=["list_relocation_targets","relocate_session"];
pub fn activation_names(active:&[String],wanted:bool)->Vec<String> {
    let mut next=active.iter().filter(|name|!RELOCATION_TOOLS.contains(&name.as_str())).cloned().collect::<Vec<_>>();
    if wanted{next.extend(RELOCATION_TOOLS.map(str::to_owned));}
    next
}
pub fn sync_activation(actions:&dyn ExtensionSessionActions)->Result<(),ExtensionFailure> {
    let wanted=actions.relocation_offered();
    let active=actions.get_active_tools()?;
    let next=activation_names(&active,wanted);
    if next.len()!=active.len() || !next.iter().all(|name|active.contains(name)){actions.set_active_tools(next)?;}
    Ok(())
}
pub struct RelocateSession;
impl Extension for RelocateSession {
 fn register(&self,api:&mut ExtensionApi) {
    let runtime=api.runtime.clone();
    api.on(EventKind::SessionStart,Arc::new(move |_,_| {let runtime=runtime.clone();Box::pin(async move {sync_activation(runtime.session_actions()?.as_ref())?;Ok(EventResult::None)})}));
    let runtime=api.runtime.clone();
    api.on_with_options(EventKind::BeforeAgentStart,Arc::new(move |event,_| {let runtime=runtime.clone();Box::pin(async move {
        if let ExtensionEvent::BeforeAgentStart(event)=event && !event.preview{sync_activation(runtime.session_actions()?.as_ref())?;}
        Ok(EventResult::None)
    })}),ExtensionHandlerOptions{preview_safe:true,..Default::default()});
    let runtime=api.runtime.clone();
    let mut list=ToolDefinition::new("list_relocation_targets","List the user's other computers this session could continue on: name, online status, capacity, and whether it is about to sleep. Read-only.",json!({"type":"object","properties":{},"additionalProperties":false}),Arc::new(move |call| {let runtime=runtime.clone();Box::pin(async move {
        let actions=runtime.session_actions().map_err(|e|ToolError::Message(e.to_string()))?;
        let result=actions.relocation_targets(call.signal).await.map_err(|e|ToolError::Message(e.to_string()))?;
        Ok(ToolResult{content:vec![ToolContent::text(targets_text(&result))],details:Some(serde_json::to_value(result).map_err(|e|ToolError::Message(e.to_string()))?),..Default::default()})
    })}));
    list.label="Other Computers".into();list.exposure=Some(ToolExposure::Search);list.allow_lazy_activation=Some(false);api.register_tool(list);
    let runtime=api.runtime.clone();
    let mut relocate=ToolDefinition::new("relocate_session","Continue this session on another of the user's computers. The current turn is never cut short: the move happens when this turn ends; the conversation, its id and queued messages continue there. Name a computer from list_relocation_targets. If unavailable, keep working here.",json!({"type":"object","properties":{"target":{"type":"string","description":"Name of the computer from list_relocation_targets."},"reason":{"type":"string","description":"Why, in a short sentence the user will see."}},"required":["target","reason"],"additionalProperties":false}),Arc::new(move |call| {let runtime=runtime.clone();Box::pin(async move {
        let request=serde_json::from_value(call.params).map_err(|e|ToolError::Message(e.to_string()))?;
        let actions=runtime.session_actions().map_err(|e|ToolError::Message(e.to_string()))?;
        let result=actions.request_relocation(request,call.signal).await.map_err(|e|ToolError::Message(e.to_string()))?;
        Ok(ToolResult{content:vec![ToolContent::text(request_text(&result))],details:Some(serde_json::to_value(result).map_err(|e|ToolError::Message(e.to_string()))?),..Default::default()})
    })}));
    relocate.label="Continue on Another Computer".into();relocate.exposure=Some(ToolExposure::Search);relocate.allow_lazy_activation=Some(false);api.register_tool(relocate);
 }
}
#[cfg(test)] mod tests {
 use super::*;
 #[test] fn activation_tracks_host_offer_without_changing_other_tools(){
    let active=vec!["read".into()];
    let offered=activation_names(&active,true);
    assert_eq!(offered,vec!["read","list_relocation_targets","relocate_session"]);
    assert_eq!(activation_names(&offered,true),offered);
    assert_eq!(activation_names(&offered,false),active);
 }
 #[test] fn result_details_keep_host_status(){
    for result in [RelocationRequestResult::Scheduled{target:"desktop".into(),move_id:"move".into()},RelocationRequestResult::Unavailable{reason:"not_offered".into(),detail:"offline".into()},RelocationRequestResult::Refused{reason:"busy".into(),detail:"busy".into()}] {
        let value=serde_json::to_value(&result).unwrap();
        assert_eq!(serde_json::from_value::<RelocationRequestResult>(value).unwrap(),result);
    }
 }
}
