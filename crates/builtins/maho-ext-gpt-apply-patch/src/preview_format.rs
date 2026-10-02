use crate::{types::{ApplyPatchOperation,ApplyPatchPreview,ApplyPatchPreviewFile},workspace::resolve_patch_path};
pub const PATCH_PREVIEW_MAX_LINES:usize=16;
pub const PATCH_PREVIEW_MAX_CHARS:usize=4000;
#[derive(Default)]
pub struct ApplyPatchRenderState { pub cwd:String,pub patch_text:String,pub call_text:String,pub collapsed:String,pub expanded:String,pub streaming:std::sync::Arc<std::sync::Mutex<crate::streaming_render::StreamingRenderState>> }
type RenderStates=std::sync::Mutex<std::collections::BTreeMap<String,std::sync::Arc<std::sync::Mutex<ApplyPatchRenderState>>>>;
static RENDER_STATES:std::sync::LazyLock<RenderStates>=std::sync::LazyLock::new(||std::sync::Mutex::new(std::collections::BTreeMap::new()));
pub fn get_apply_patch_render_state(tool_call_id:&str,cwd:&str,patch_text:&str)->std::sync::Arc<std::sync::Mutex<ApplyPatchRenderState>> {
    let mut states=RENDER_STATES.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(existing)=states.get(tool_call_id) { let state=existing.lock().unwrap_or_else(std::sync::PoisonError::into_inner); if state.cwd==cwd && state.patch_text==patch_text { return existing.clone(); } }
    let streaming=states.get(tool_call_id).map(|existing|existing.lock().unwrap_or_else(std::sync::PoisonError::into_inner).streaming.clone()).unwrap_or_default();
    let mut state=ApplyPatchRenderState{cwd:cwd.into(),patch_text:patch_text.into(),call_text:format_in_flight_call_text(patch_text),streaming,..Default::default()};
    if let Ok(hunks)=crate::parser::parse_patch(patch_text) && !hunks.is_empty() {
        let files=hunks.into_iter().map(|hunk| { let (file_path,move_path,operation)=match hunk { crate::types::ParsedPatch::Add{file_path,..}=>(file_path,None,ApplyPatchOperation::Add),crate::types::ParsedPatch::Delete{file_path}=>(file_path,None,ApplyPatchOperation::Delete),crate::types::ParsedPatch::Update{file_path,move_path,..}=>(file_path,move_path,ApplyPatchOperation::Update) }; ApplyPatchPreviewFile{file_path,move_path,operation,diff:String::new(),patch:None,binary:None,added:0,removed:0} }).collect();
        let preview=ApplyPatchPreview{files,added:0,removed:0}; state.collapsed=format_patch_preview(&preview,cwd,false); state.expanded=format_patch_preview(&preview,cwd,true);
    }
    let state=std::sync::Arc::new(std::sync::Mutex::new(state)); states.insert(tool_call_id.into(),state.clone()); state
}
pub fn clear_apply_patch_render_state() { RENDER_STATES.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear(); }
fn js_whitespace(character:char)->bool { matches!(character,'\u{0009}'..='\u{000d}'|'\u{0020}'|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}') }
pub fn format_in_flight_call_text(patch_text:&str)->String {
    let paths=crate::text::extract_patched_paths(patch_text);
    if paths.is_empty() { return "Patching".into(); }
    let count=if paths.len()>1 { format!(" ({} files)",paths.len()) } else { String::new() };
    format!("Patching{count}: {}",paths.join(", "))
}
fn changed(line:&str)->bool {
    let Some(first)=line.chars().next() else { return false; }; if first!='+' && first!='-' { return false; }
    let mut chars=line[first.len_utf8()..].chars().peekable(); while chars.peek().is_some_and(|c|js_whitespace(*c)) { chars.next(); }
    let mut digits=0; while chars.peek().is_some_and(|c|c.is_ascii_digit()) { chars.next(); digits+=1; }
    digits>0 && chars.next().is_some_and(js_whitespace)
}
fn window_count(len:usize,start:usize,end:usize)->usize { end-start+usize::from(start>0)+usize::from(end<len) }
pub fn truncate_preview(text:&str)->String {
    if text.encode_utf16().count()<=PATCH_PREVIEW_MAX_CHARS && (if text.is_empty() { 0 } else { text.bytes().filter(|b|*b==b'\n').count()+1 })<=PATCH_PREVIEW_MAX_LINES { return text.into(); }
    let lines:Vec<_>=text.split('\n').collect();
    let preview=if let Some(first)=lines.iter().position(|line|changed(line)) {
        let mut start=first; let mut end=first+1; while end<lines.len() && changed(lines[end]) { end+=1; }
        let hunk_end=end; while end>start && window_count(lines.len(),start,end)>PATCH_PREVIEW_MAX_LINES { end-=1; }
        while window_count(lines.len(),start,end)<PATCH_PREVIEW_MAX_LINES {
            let before=start>0; let after=end<lines.len(); if !before && !after { break; }
            if before && (!after || (first-start) as isize <= end as isize-hunk_end as isize) { start-=1; } else { end+=1; }
        }
        let mut result=Vec::new(); if start>0 { result.push("…"); } result.extend_from_slice(&lines[start..end]); if end<lines.len() { result.push("…"); } result.join("\n")
    } else { lines.iter().take(8).copied().chain(std::iter::once("…")).chain(lines.iter().skip(lines.len().saturating_sub(7)).copied()).collect::<Vec<_>>().join("\n") };
    if preview.encode_utf16().count()<=PATCH_PREVIEW_MAX_CHARS { return preview; }
    let mut units=0; let mut end=0; for (index,c) in preview.char_indices() { if units+c.len_utf16()>PATCH_PREVIEW_MAX_CHARS-1 { break; } units+=c.len_utf16(); end=index+c.len_utf8(); }
    format!("{}…",preview[..end].trim_end_matches(js_whitespace))
}
pub fn display_path(file_path:&str,cwd:&str)->String {
    let path=std::path::Path::new(file_path); if !path.is_absolute() { return file_path.into(); }
    let base=resolve_patch_path(std::path::Path::new(cwd),std::path::Path::new(""));
    let normalized=resolve_patch_path(std::path::Path::new(""),path);
    match normalized.strip_prefix(&base) { Ok(relative)=>if relative.as_os_str().is_empty() { ".".into() } else { relative.to_string_lossy().into_owned() },Err(_)=>file_path.into() }
}
fn file_path(file:&ApplyPatchPreviewFile,cwd:&str)->String {
    let path=display_path(&file.file_path,cwd); match file.move_path.as_deref().filter(|path|!path.is_empty()) { Some(destination)=>format!("{path} → {}",display_path(destination,cwd)),None=>path }
}
fn summary(file:&ApplyPatchPreviewFile)->String { if file.binary==Some(true) { "(binary)".into() } else { format!("(+{} -{})",file.added,file.removed) } }
pub fn format_patch_preview(preview:&ApplyPatchPreview,cwd:&str,expanded:bool)->String {
    let mut lines=Vec::new();
    if preview.files.len()==1 { let file=&preview.files[0]; let operation=match file.operation { ApplyPatchOperation::Add=>"Added",ApplyPatchOperation::Delete=>"Deleted",ApplyPatchOperation::Update=>"Edited" }; lines.push(format!("• {operation} {} {}",file_path(file,cwd),summary(file))); if expanded && !file.diff.is_empty() { lines.extend(truncate_preview(&file.diff).split('\n').map(|line|format!("  {line}"))); } }
    else { lines.push(format!("• Edited {} files (+{} -{})",preview.files.len(),preview.added,preview.removed)); for file in &preview.files { lines.push(format!("  └ {} {}",file_path(file,cwd),summary(file))); if expanded && !file.diff.is_empty() { lines.extend(truncate_preview(&file.diff).split('\n').map(|line|format!("    {line}"))); } } }
    lines.join("\n")
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn render_state_identity_and_stream_are_retained_by_call_id() {
        let patch="*** Begin Patch\n*** Add File: a\n+one\n*** End Patch";
        let first=get_apply_patch_render_state("cache-identity","/root",patch); first.lock().unwrap().streaming.lock().unwrap().update("*** Begin Patch\n*** Add File: a\n");
        assert!(std::sync::Arc::ptr_eq(&first,&get_apply_patch_render_state("cache-identity","/root",patch)));
        let next=get_apply_patch_render_state("cache-identity","/other",patch); assert!(!std::sync::Arc::ptr_eq(&first,&next)); assert_eq!(next.lock().unwrap().streaming.lock().unwrap().hunks.len(),1); assert_eq!(first.lock().unwrap().streaming.lock().unwrap().hunks.len(),1);
        RENDER_STATES.lock().unwrap().remove("cache-identity");
    }
    #[test] fn absolute_dot_segments_are_normalized_only_inside_cwd() { assert_eq!(display_path("/root/other/../src/a","/root"),"src/a"); assert_eq!(display_path("/root/../elsewhere/a","/root"),"/root/../elsewhere/a"); }
    #[test] fn changed_hunk_is_kept_in_line_window() { let lines=(1..=40).map(|i|format!("{}{:2} line",if i==30 { '+' } else { ' ' },i)).collect::<Vec<_>>().join("\n"); let preview=truncate_preview(&lines); assert!(preview.contains("+30 line")); assert_eq!(preview.lines().count(),16); assert!(preview.starts_with('…')); }
    #[test] fn plain_text_uses_head_and_tail() { let lines=(0..20).map(|i|i.to_string()).collect::<Vec<_>>().join("\n"); let preview=truncate_preview(&lines); assert_eq!(preview.lines().count(),16); assert!(preview.contains("7\n…\n13")); }
    #[test] fn character_limit_counts_utf16() { let preview=truncate_preview(&"a".repeat(5000)); assert_eq!(preview.encode_utf16().count(),4000); assert!(preview.ends_with('…')); }
    #[test] fn path_inside_cwd_becomes_relative() { assert_eq!(display_path("/root/src/a","/root"),"src/a"); assert_eq!(display_path("/other/a","/root"),"/other/a"); assert_eq!(display_path("/root","/root"),"."); }
}
