use comment_checker_core::{HookInput,HookToolInput,EditPair};
use maho_ext_api::{ExtensionContext,ToolResultEvent};

pub fn to_hook_input(event:&ToolResultEvent,ctx:&ExtensionContext,absolute_path:&str) -> HookInput {
    let input=&event.input;
    let edits=input["edits"].as_array().map(|edits|edits.iter().filter_map(|edit|Some(EditPair{
        old_string:edit["old_string"].as_str().or_else(||edit["oldText"].as_str())?.into(),
        new_string:edit["new_string"].as_str().or_else(||edit["newText"].as_str())?.into(),
    })).collect::<Vec<_>>()).filter(|edits|!edits.is_empty());
    HookInput{
        session_id:ctx.session_manager.session_id().into(),tool_name:event.tool_name.clone(),
        transcript_path:ctx.session_manager.session_file().map(|p|p.to_string_lossy().into_owned()).unwrap_or_default(),
        cwd:ctx.cwd.to_string_lossy().into_owned(),hook_event_name:"PostToolUse".into(),
        tool_input:HookToolInput{file_path:Some(absolute_path.into()),content:input["content"].as_str().map(str::to_owned),
            old_string:input["old_string"].as_str().or_else(||input["oldText"].as_str()).map(str::to_owned),
            new_string:input["new_string"].as_str().or_else(||input["newText"].as_str()).map(str::to_owned),edits},
        tool_response:Some(serde_json::json!({"content":event.content,"details":event.details,"isError":event.is_error})),
    }
}
