use comment_checker_core::{HookInput,HookToolInput,EditPair,CheckerEdit,extract_apply_patch_edits};
use maho_ext_api::{ExtensionContext,ToolResultEvent};

pub fn to_hook_input(event:&ToolResultEvent,ctx:&ExtensionContext,absolute_path:&str) -> HookInput {
    let input=&event.input;
    let edits=input["edits"].as_array().map(|edits|edits.iter().filter_map(|edit|Some(EditPair{
        old_string:edit["old_string"].as_str().or_else(||edit["oldText"].as_str())?.into(),
        new_string:edit["new_string"].as_str().or_else(||edit["newText"].as_str())?.into(),
    })).collect::<Vec<_>>()).filter(|edits|!edits.is_empty());
    HookInput{
        session_id:match ctx.session_manager.session_id() { "" => "unknown".into(), id => id.into() },tool_name:event.tool_name.clone(),
        transcript_path:ctx.session_manager.session_file().map(|p|p.to_string_lossy().into_owned()).unwrap_or_default(),
        cwd:ctx.cwd.to_string_lossy().into_owned(),hook_event_name:"PostToolUse".into(),
        tool_input:HookToolInput{file_path:Some(absolute_path.into()),content:input["content"].as_str().map(str::to_owned),
            old_string:input["old_string"].as_str().or_else(||input["oldText"].as_str()).map(str::to_owned),
            new_string:input["new_string"].as_str().or_else(||input["newText"].as_str()).map(str::to_owned),edits},
        tool_response:Some(serde_json::json!({"content":event.content,"details":event.details,"isError":event.is_error})),
    }
}

pub fn to_apply_patch_hook_inputs(event: &ToolResultEvent, ctx: &ExtensionContext) -> Vec<(CheckerEdit, HookInput)> {
    extract_apply_patch_edits(
        event.details.as_ref().unwrap_or(&serde_json::Value::Null),
        event.input.as_object(),
    )
    .into_iter()
    .map(|edit| {
        let path = ctx.cwd.join(&edit.file_path);
        let absolute = normalize_path(&path);
        let mut hook = to_hook_input(event, ctx, &absolute.to_string_lossy());
        hook.tool_input.content = Some(edit.after.clone());
        hook.tool_input.old_string = Some(edit.before.clone());
        (edit, hook)
    })
    .collect()
}

fn normalize_path(path: &std::path::Path) -> std::path::PathBuf {
    let mut absolute = std::path::PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::CurDir => {},
            std::path::Component::ParentDir => { absolute.pop(); },
            other => absolute.push(other.as_os_str()),
        }
    }
    absolute
}
