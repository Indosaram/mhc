use base64::{Engine, engine::general_purpose::STANDARD};
use maho_ext_api::{AbortSignal, Extension, ExtensionApi, Model, ToolContent, ToolDefinition, ToolResult};
use maho_tools::definition::ToolError;
use serde::Deserialize;
use serde_json::json;
use std::{path::Path, sync::Arc};
use maho_ext_api::{EventKind,ExtensionEvent,EventResult};

pub const MAX_VIDEO_BYTES: u64 = 100 * 1024 * 1024;
const EXTENSIONS: &str = "mp4, mpeg, mpg, mov, webm, mkv, avi, flv, 3gp";

pub fn detect_video_mime_type(path: &Path) -> Option<&'static str> {
    match path.extension()?.to_str()?.to_lowercase().as_str() {
        "mp4" => Some("video/mp4"), "mpeg" | "mpg" => Some("video/mpeg"),
        "mov" => Some("video/quicktime"), "webm" => Some("video/webm"),
        "mkv" => Some("video/x-matroska"), "avi" => Some("video/x-msvideo"),
        "flv" => Some("video/x-flv"), "3gp" => Some("video/3gpp"), _ => None,
    }
}
pub fn model_supports_video(model: Option<&Model>) -> bool {
    model.is_some_and(|model| model.input.contains(&maho_ai::types::InputModality::Video))
}
pub fn activation_change(active:&[String],model:Option<&Model>)->Option<Vec<String>>{
    let is_active=active.iter().any(|name|name=="read_video");
    match (model_supports_video(model),is_active){
        (true,false)=>{let mut names=active.to_vec();names.push("read_video".into());Some(names)},
        (false,true)=>Some(active.iter().filter(|name|name.as_str()!="read_video").cloned().collect()),
        _=>None,
    }
}

#[derive(Deserialize)]
struct Params { path: String }

pub async fn read_video(path: &Path, signal: &AbortSignal) -> Result<ToolResult, ToolError> {
    signal.check()?;
    let mime = detect_video_mime_type(path).ok_or_else(|| ToolError::Message(format!("\"{}\" is not a supported video file. Supported extensions: {EXTENSIONS}.",path.display())))?;
    let stats = tokio::fs::metadata(path).await?;
    if !stats.is_file() { return Err(ToolError::Message(format!("\"{}\" is not a regular file.",path.display()))); }
    if stats.len() == 0 { return Err(ToolError::Message(format!("\"{}\" is empty.",path.display()))); }
    if stats.len() > MAX_VIDEO_BYTES { return Err(ToolError::Message(format!("\"{}\" is {} bytes, which exceeds the maximum 100MB for video files. Create a smaller clip (e.g. with ffmpeg) and read that instead.",path.display(),stats.len()))); }
    let data = tokio::fs::read(path).await?;
    signal.check()?;
    Ok(ToolResult { content: vec![ToolContent::text(format!("Read video file \"{}\" [{mime}, {} bytes]. The video is attached below.",path.file_name().unwrap_or_default().to_string_lossy(),stats.len())), ToolContent::Image { data: STANDARD.encode(data), mime_type: mime.into() }], details: None })
}

pub struct VideoIn;
impl Extension for VideoIn {
    fn register(&self, api: &mut ExtensionApi) {
        let mut tool = ToolDefinition::new("read_video", &format!("Read a video file ({EXTENSIONS}) and attach it to the conversation so you can watch it. Maximum file size 100MB. Use this to understand screen recordings, demo clips, or any behavior that is hard to describe in text. If you generate or edit a video via commands or scripts, read the result back before continuing."), json!({"type":"object","properties":{"path":{"type":"string","description":"Path to a video file (relative or absolute). Max 100MB."}},"required":["path"]}), Arc::new(|call| Box::pin(async move {
            call.signal.check()?;
            let ctx = call.context.ok_or_else(|| ToolError::Message("Tool context unavailable".into()))?;
            if !model_supports_video(ctx.model()) { return Err(ToolError::Message("The current model does not support video input. Tell the user to switch to a model with video input capability (e.g. kimi-coding/k3).".into())); }
            let params: Params = serde_json::from_value(call.params)?;
            let path = Path::new(&params.path);
            let absolute = if path.is_absolute() { path.to_owned() } else { ctx.cwd().join(path) };
            read_video(&absolute, &call.signal).await
        })));
        tool.label = "Read Video".into();
        tool.prompt_snippet = Some("Attach a video file so the model can watch it (video-capable models only)".into());
        api.register_tool(tool);
        for kind in [EventKind::SessionStart,EventKind::ModelSelect]{
            let runtime=api.runtime.clone();
            api.on(kind,Arc::new(move|event,ctx|{
                let runtime=runtime.clone();
                Box::pin(async move{
                    let model=match event{ExtensionEvent::ModelSelect(event)=>Some(&event.model),_=>ctx.model.as_ref()};
                    let actions=runtime.session_actions()?;
                    if let Some(names)=activation_change(&actions.get_active_tools()?,model){actions.set_active_tools(names)?;}
                    Ok(EventResult::None)
                })
            }));
        }
    }
}
