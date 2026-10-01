use crate::{evaluate::evaluate, events::{PermissionEventEmitter, PermissionRepliedEvent}, types::{Action, PermissionDecision, Reply, ReplyInput, Request, Rule}};

pub fn handle_no_ui(request: &Request, rulesets: (&[Rule], &[Rule]), emitter: &PermissionEventEmitter) -> Result<Option<ReplyInput>, serde_json::Error> {
    emitter.emit_asked(request)?;
    let pattern=request.patterns.first().map_or("",String::as_str);
    for (rules,source) in [(rulesets.1,"CLI flag"),(rulesets.0,"config")] {
        match evaluate(&request.permission,pattern,&[rules]).action {
            Action::Allow => { emitter.emit_replied(PermissionRepliedEvent {request_id:request.id.clone(),session_id:request.session_id.clone(),reply:PermissionDecision::Allow})?; return Ok(None); }
            Action::Deny => return Ok(Some(ReplyInput {request_id:request.id.clone(),reply:Reply::Reject,message:Some(format!("Permission denied by {source}: {}",request.permission))})),
            Action::Ask => {},
        }
    }
    Ok(Some(ReplyInput {request_id:request.id.clone(),reply:Reply::Reject,message:Some(format!("Permission required for {} ({}). Use --permission {}=allow to override.",request.permission,request.patterns.join(", "),request.permission))}))
}
