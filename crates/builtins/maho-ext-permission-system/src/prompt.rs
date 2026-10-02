use crate::types::{Reply,ReplyInput,Request};
use maho_ext_api::{ExtensionContext,ExtensionUiDialogOptions};
use serde_json::Value;

fn display(value:Option<&Value>)->String {value.filter(|value| !value.is_null() && **value != false && **value != "").map_or_else(||"Unknown".into(),|value|value.as_str().map_or_else(||value.to_string(),str::to_owned))}
pub fn format_request_for_display(request:&Request)->String {
    let meta=&request.metadata;
    let mut parts=Vec::new();
    match request.permission.as_str() {
        "edit"=>parts.push(format!("File: {}",display(meta.get("filepath")))),
        "read"=>parts.push(format!("Path: {}",display(meta.get("filePath")))),
        "glob"|"grep"=>parts.push(format!("Pattern: {}",display(meta.get("pattern")))),
        "list"=>parts.push(format!("Path: {}",display(meta.get("path")))),
        "bash"=>{if let Some(description)=meta.get("description").filter(|value| !value.is_null() && **value != "") {parts.push(format!("Description: {}",display(Some(description))));} parts.push(format!("Command: $ {}",display(meta.get("command"))));}
        "websearch"|"codesearch"=>parts.push(format!("Query: {}",display(meta.get("query")))),
        "external_directory"=>{let directory=meta.get("parentDir").and_then(Value::as_str).or_else(||meta.get("filepath").and_then(Value::as_str)).map(str::to_owned).or_else(||request.patterns.first().map(|pattern|pattern.split('*').next().unwrap_or(pattern).to_owned())).unwrap_or_else(||"Unknown".into());parts.push(format!("Directory: {directory}"));}
        _=>parts.push(format!("Tool: {}",request.permission)),
    }
    if !request.patterns.is_empty(){parts.push(format!("\nPatterns:\n{}",request.patterns.iter().map(|pattern|format!("  - {pattern}")).collect::<Vec<_>>().join("\n")));}
    parts.join("\n")
}
pub async fn show_permission_prompt(ctx:&ExtensionContext,request:&Request)->ReplyInput {
    let title=format!("Permission required: {}\n\n{}",request.permission,format_request_for_display(request));
    let options=["Allow once","Allow always","Deny","Deny with feedback"].map(str::to_owned);
    let choice=ctx.ui.select(&title,&options,ExtensionUiDialogOptions::default()).await;
    let (reply,message)=match choice.as_deref(){
        Some("Allow once")=>(Reply::Once,None),Some("Allow always")=>(Reply::Always,None),
        Some("Deny with feedback")=>(Reply::Reject,ctx.ui.input("Feedback",Some("Why are you denying this permission? (optional)"),ExtensionUiDialogOptions::default()).await.filter(|message|!message.is_empty())),
        _=>(Reply::Reject,None),
    };
    ReplyInput{request_id:request.id.clone(),reply,message}
}
