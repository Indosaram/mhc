use std::path::Path;
use crate::{types::{ParsedPatch,ApplyPatchOperation,ApplyPatchPreviewFile,ApplyPatchPreview},patch_diff::create_patch_diff,patch_replace::replace_chunks,workspace::resolve_patch_path};
#[derive(Clone,Debug,Default,PartialEq,Eq)]
pub struct PatchFileSnapshot { pub exists:bool,pub content:String,pub binary:bool,pub bytes:Option<Vec<u8>> }
pub async fn read_patch_file_snapshot(path:&Path)->Result<PatchFileSnapshot,std::io::Error> {
    let bytes=match tokio::fs::read(path).await { Ok(bytes)=>bytes,Err(error) if error.kind()==std::io::ErrorKind::NotFound=>return Ok(PatchFileSnapshot::default()),Err(error)=>return Err(error) };
    if bytes.contains(&0) { return Ok(PatchFileSnapshot{exists:true,content:String::new(),binary:true,bytes:Some(bytes)}); }
    match String::from_utf8(bytes) { Ok(content)=>Ok(PatchFileSnapshot{exists:true,content:content.strip_prefix('\u{feff}').unwrap_or(&content).into(),binary:false,bytes:None}),Err(error)=>Ok(PatchFileSnapshot{exists:true,content:String::new(),binary:true,bytes:Some(error.into_bytes())}) }
}
pub fn build_patch_preview_file(hunk:&ParsedPatch,source:&PatchFileSnapshot,new_content:&str,destination:Option<&PatchFileSnapshot>)->ApplyPatchPreviewFile {
    let (path,move_path,operation)=match hunk { ParsedPatch::Add{file_path,..}=>(file_path,None,if source.exists { ApplyPatchOperation::Update } else { ApplyPatchOperation::Add }),ParsedPatch::Delete{file_path}=>(file_path,None,ApplyPatchOperation::Delete),ParsedPatch::Update{file_path,move_path,..}=>(file_path,move_path.clone(),ApplyPatchOperation::Update) };
    if source.binary || destination.is_some_and(|destination|destination.binary) { return ApplyPatchPreviewFile{file_path:path.clone(),move_path:move_path.filter(|path|!path.is_empty()),operation,binary:Some(true),diff:String::new(),patch:None,added:0,removed:0}; }
    let unified=|old_path:&str,new_path:&str,old:&str,new:&str|maho_tools::unified_diff::create_unified_patch(old_path,new_path,old,new,4);
    let patch=match hunk {
        ParsedPatch::Add{..}=>unified(if operation==ApplyPatchOperation::Add { "/dev/null" } else { path },path,&source.content,new_content),
        ParsedPatch::Delete{..}=>unified(path,"/dev/null",&source.content,""),
        ParsedPatch::Update{..}=>match move_path.as_deref().filter(|destination|!destination.is_empty() && *destination!=path) {
            Some(move_path)=>{
                let source_patch=unified(&format!("a/{path}"),"/dev/null",&source.content,"");
                let destination_patch=if let Some(destination)=destination.filter(|destination|destination.exists) { if destination.content==new_content { String::new() } else { unified(&format!("a/{move_path}"),&format!("b/{move_path}"),&destination.content,new_content) } } else { unified("/dev/null",&format!("b/{move_path}"),"",new_content) };
                format!("{source_patch}{destination_patch}")
            },None=>unified(path,move_path.as_deref().unwrap_or(path),&source.content,new_content),
        },
    };
    let diff=create_patch_diff(&source.content,new_content);
    ApplyPatchPreviewFile{file_path:path.clone(),move_path,operation,binary:None,diff:diff.diff,patch:Some(patch),added:diff.added,removed:diff.removed}
}
pub async fn create_patch_preview(cwd:&Path,hunks:&[ParsedPatch])->ApplyPatchPreview {
    let mut preview=ApplyPatchPreview::default();
    for hunk in hunks {
        let file=async {
            let path=match hunk { ParsedPatch::Add{file_path,..}|ParsedPatch::Delete{file_path}|ParsedPatch::Update{file_path,..}=>file_path };
            let source=read_patch_file_snapshot(&resolve_patch_path(cwd,Path::new(path))).await.map_err(|error|error.to_string())?;
            let destination=if let ParsedPatch::Update{move_path:Some(destination),..}=hunk { if destination!=path && !destination.is_empty() { Some(read_patch_file_snapshot(&resolve_patch_path(cwd,Path::new(destination))).await.map_err(|error|error.to_string())?) } else { None } } else { None };
            let content=match hunk { ParsedPatch::Add{content,..}=>content.clone(),ParsedPatch::Delete{..}=>String::new(),ParsedPatch::Update{chunks,..}=>if source.binary || destination.as_ref().is_some_and(|destination|destination.binary) { String::new() } else if chunks.is_empty() { source.content.clone() } else { replace_chunks(&source.content,path,chunks)?.0 } };
            Ok::<_,String>(build_patch_preview_file(hunk,&source,&content,destination.as_ref()))
        }.await;
        if let Ok(file)=file { preview.added+=file.added; preview.removed+=file.removed; preview.files.push(file); }
    }
    preview
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn add_over_existing_file_is_update() { let file=build_patch_preview_file(&ParsedPatch::Add{file_path:"a".into(),content:"new\n".into()},&PatchFileSnapshot{exists:true,content:"old\n".into(),..Default::default()},"new\n",None); assert_eq!(file.operation,ApplyPatchOperation::Update); assert_eq!((file.added,file.removed),(1,1)); }
    #[test] fn binary_preview_has_no_text_patch() { let file=build_patch_preview_file(&ParsedPatch::Delete{file_path:"a".into()},&PatchFileSnapshot{exists:true,binary:true,..Default::default()},"",None); assert_eq!(file.binary,Some(true)); assert_eq!(file.patch,None); }
    #[test] fn move_to_matching_destination_omits_destination_patch() { let source=PatchFileSnapshot{exists:true,content:"same\n".into(),..Default::default()}; let file=build_patch_preview_file(&ParsedPatch::Update{file_path:"a".into(),move_path:Some("b".into()),chunks:vec![]},&source,"same\n",Some(&source)); let patch=file.patch.unwrap(); assert!(patch.contains("--- a/a\n+++ /dev/null")); assert!(!patch.contains("b/b")); }
}
