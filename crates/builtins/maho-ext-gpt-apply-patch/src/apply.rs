use crate::{parser::parse_patch,text::normalize_patch_text,types::{ParsedPatch,ApplyPatchResult}};
fn js_whitespace(character:char)->bool { matches!(character,'\u{0009}'..='\u{000d}'|'\u{0020}'|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}') }
pub fn parse_non_empty_patch(patch_text:&str)->Result<Vec<ParsedPatch>,String> {
    let hunks=parse_patch(patch_text)?;
    if hunks.is_empty() { return Err(if normalize_patch_text(patch_text).trim_matches(js_whitespace)=="*** Begin Patch\n*** End Patch" { "patch rejected: empty patch" } else { "apply_patch verification failed: no hunks found" }.into()); }
    Ok(hunks)
}
pub fn compact_apply_patch_result(mut result:ApplyPatchResult)->ApplyPatchResult {
    for operation in &mut result.details.applied_operations { operation.preview.diff.clear(); operation.preview.patch=None; } result
}
use std::{path::Path,sync::atomic::{AtomicU64,Ordering}};
use crate::{preview::{read_patch_file_snapshot,build_patch_preview_file},workspace::resolve_patch_path,patch_replace::replace_chunks,types::{ApplyPatchFailure,AppliedPatchOperation},recovery::create_recovery_instructions,errors::ApplyPatchError};
#[derive(Debug)]
struct MutationError { message:String,code:Option<String> }
impl From<String> for MutationError { fn from(message:String)->Self { Self{message,code:None} } }
impl From<std::io::Error> for MutationError { fn from(error:std::io::Error)->Self { Self{message:error.to_string(),code:match error.kind() { std::io::ErrorKind::NotFound=>Some("ENOENT".into()),std::io::ErrorKind::PermissionDenied=>Some(if error.raw_os_error()==Some(1) { "EPERM" } else { "EACCES" }.into()),std::io::ErrorKind::AlreadyExists=>Some("EEXIST".into()),std::io::ErrorKind::NotADirectory=>Some("ENOTDIR".into()),std::io::ErrorKind::IsADirectory=>Some("EISDIR".into()),_=>None }} } }
static TEMP_ID:AtomicU64=AtomicU64::new(0);
struct FileOperations;
impl crate::types::AtomicWriteOperations for FileOperations {
    fn write_file<'a>(&'a self,path:&'a Path,content:&'a [u8])->crate::types::AtomicWriteFuture<'a> { Box::pin(tokio::fs::write(path,content)) }
    fn rename<'a>(&'a self,from:&'a Path,to:&'a Path)->crate::types::AtomicWriteFuture<'a> { Box::pin(tokio::fs::rename(from,to)) }
    fn unlink<'a>(&'a self,path:&'a Path)->crate::types::AtomicWriteFuture<'a> { Box::pin(tokio::fs::remove_file(path)) }
}
async fn write_file_atomic(path:&Path,content:&[u8])->Result<(),MutationError> {
    write_file_atomic_with_operations(path,content,&FileOperations).await.map_err(MutationError::from)
}
pub async fn write_file_atomic_with_operations(path:&Path,content:&[u8],operations:&dyn crate::types::AtomicWriteOperations)->std::io::Result<()> {
    let temp=std::path::PathBuf::from(format!("{}.tmp.{}.{}",path.display(),std::process::id(),TEMP_ID.fetch_add(1,Ordering::Relaxed)));
    operations.write_file(&temp,content).await?;
    match operations.rename(&temp,path).await { Ok(())=>Ok(()),Err(error) if error.kind()==std::io::ErrorKind::AlreadyExists=>{ operations.unlink(path).await?; operations.rename(&temp,path).await?; Ok(()) },Err(error)=>Err(error) }
}
async fn apply_single_hunk(cwd:&Path,hunk:&ParsedPatch)->Result<(String,String,usize,crate::types::ApplyPatchPreviewFile),MutationError> {
    let file=match hunk { ParsedPatch::Add{file_path,..}|ParsedPatch::Delete{file_path}|ParsedPatch::Update{file_path,..}=>file_path };
    let path=resolve_patch_path(cwd,Path::new(file));
    let move_path=if let ParsedPatch::Update{move_path:Some(destination),..}=hunk { if destination.is_empty() { None } else { Some(resolve_patch_path(cwd,Path::new(destination))) } } else { None };
    let mut paths=vec![path.clone()]; if let Some(destination)=&move_path { paths.push(destination.clone()); } paths.sort(); paths.dedup();
    let mut guards=Vec::new(); for path in paths { guards.push(maho_tools::file_mutation_queue::lock_file_mutation(&path).await.map_err(|error|MutationError::from(error.to_string()))?); }
    let source=read_patch_file_snapshot(&path).await?;
    match hunk {
        ParsedPatch::Add{content,..}=>{ let preview=build_patch_preview_file(hunk,&source,content,None); tokio::fs::create_dir_all(path.parent().expect("resolved parent")).await?; write_file_atomic(&path,content.as_bytes()).await?; Ok((format!("add: {file}"),file.clone(),0,preview)) },
        ParsedPatch::Delete{..}=>{ let preview=build_patch_preview_file(hunk,&source,"",None); tokio::fs::remove_file(&path).await?; Ok((format!("delete: {file}"),file.clone(),0,preview)) },
        ParsedPatch::Update{chunks,move_path:destination,..}=>{
            if !source.exists { return Err(MutationError{message:format!("ENOENT: no such file or directory, open '{}'",path.display()),code:Some("ENOENT".into())}); }
            let move_destination=match &move_path { Some(destination) if destination!=&path=>Some(read_patch_file_snapshot(destination).await?),_=>None };
            if source.binary {
                if !chunks.is_empty() { return Err(format!("apply_patch cannot apply text hunks to binary file: {file}").into()); }
                let Some(destination_path)=&move_path else { return Err(format!("apply_patch cannot update binary file without a move destination: {file}").into()); };
                let preview=build_patch_preview_file(hunk,&source,"",move_destination.as_ref());
                tokio::fs::create_dir_all(destination_path.parent().expect("resolved parent")).await?;
                write_file_atomic(destination_path,source.bytes.as_deref().expect("binary snapshot bytes")).await?;
                if destination_path!=&path { tokio::fs::remove_file(&path).await?; }
                let destination=destination.as_ref().expect("move destination"); return Ok((format!("move: {file} -> {destination}"),destination.clone(),0,preview));
            }
            let (content,fuzz)=if chunks.is_empty() { (source.content.clone(),0) } else { replace_chunks(&source.content,file,chunks)? };
            let fuzz=fuzz as usize;
            let preview=build_patch_preview_file(hunk,&source,&content,move_destination.as_ref());
            if let Some(destination_path)=move_path { tokio::fs::create_dir_all(destination_path.parent().expect("resolved parent")).await?; write_file_atomic(&destination_path,content.as_bytes()).await?; if destination_path!=path { tokio::fs::remove_file(&path).await?; } let destination=destination.as_ref().expect("move destination"); Ok((format!("move: {file} -> {destination}"),destination.clone(),fuzz,preview)) }
            else { write_file_atomic(&path,content.as_bytes()).await?; Ok((format!("update: {file}"),file.clone(),fuzz,preview)) }
        },
    }
}
pub type ApplyPatchProgressCallback<'a>=dyn Fn(crate::types::ApplyPatchProgress)->std::pin::Pin<Box<dyn std::future::Future<Output=Result<(),String>>+Send+'a>>+Send+Sync+'a;
pub async fn apply_patch_detailed(cwd:&Path,patch_text:&str)->Result<ApplyPatchResult,String> { apply_patch_detailed_with_progress(cwd,patch_text,None).await }
pub async fn apply_patch_detailed_with_progress(cwd:&Path,patch_text:&str,on_progress:Option<&ApplyPatchProgressCallback<'_>>)->Result<ApplyPatchResult,String> { apply_hunks(cwd,parse_non_empty_patch(patch_text)?,false,on_progress).await.map_err(|error|error.to_string()) }
pub async fn apply_patch(cwd:&Path,patch_text:&str)->Result<Vec<String>,ApplyPatchError> { let hunks=parse_non_empty_patch(patch_text).map_err(|message|ApplyPatchError::new(message,ApplyPatchResult::default()))?; Ok(apply_hunks(cwd,hunks,true,None).await?.summaries) }
async fn apply_hunks(cwd:&Path,hunks:Vec<ParsedPatch>,fail_fast:bool,on_progress:Option<&ApplyPatchProgressCallback<'_>>)->Result<ApplyPatchResult,ApplyPatchError> {
    let total=hunks.len();
    let mut result=ApplyPatchResult::default();
    for (operation_index,hunk) in hunks.into_iter().enumerate() {
        match apply_single_hunk(cwd,&hunk).await {
            Ok((summary,file,fuzz,preview))=>{ result.summaries.push(summary); result.applied_files.push(file); result.details.fuzz+=fuzz; result.details.applied_operations.push(AppliedPatchOperation{operation_index,preview}); },
            Err(error)=>{
                let (file_path,operation)=match hunk { ParsedPatch::Add{file_path,..}=>(file_path,crate::types::ApplyPatchOperation::Add),ParsedPatch::Delete{file_path}=>(file_path,crate::types::ApplyPatchOperation::Delete),ParsedPatch::Update{file_path,..}=>(file_path,crate::types::ApplyPatchOperation::Update) };
                result.failures.push(ApplyPatchFailure{operation_index,file_path,operation,message:error.message.clone(),code:error.code});
                if fail_fast { result.has_partial_success = !result.applied_files.is_empty(); result.recovery_instructions=create_recovery_instructions(&result.applied_files,&result.failures); result.details.fuzz=0; return Err(ApplyPatchError::new(error.message,compact_apply_patch_result(result))); }
            },
        }
        if let Some(on_progress)=on_progress { let _=on_progress(crate::types::ApplyPatchProgress{applied:result.applied_files.len(),failed:result.failures.len(),total}).await; }
    }
    result.has_partial_success = !result.applied_files.is_empty() && !result.failures.is_empty(); result.recovery_instructions=create_recovery_instructions(&result.applied_files,&result.failures); Ok(result)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AppliedPatchOperation,ApplyPatchPreviewFile,ApplyPatchOperation};
    #[tokio::test] async fn add_file_uses_atomic_write_for_nested_and_absolute_paths() {
        let workspace=tempfile::tempdir().unwrap();let outside=tempfile::tempdir().unwrap();
        let summaries=apply_patch(workspace.path(),"*** Begin Patch\n*** Add File: nested/new.txt\n+hello\n*** End Patch").await.unwrap();
        assert_eq!(summaries,["add: nested/new.txt"]);assert_eq!(tokio::fs::read(workspace.path().join("nested/new.txt")).await.unwrap(),b"hello\n");
        let path=outside.path().join("outside.txt");let patch=format!("*** Begin Patch\n*** Add File: {}\n+outside\n*** End Patch",path.display());
        apply_patch(workspace.path(),&patch).await.unwrap();assert_eq!(tokio::fs::read(&path).await.unwrap(),b"outside\n");assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(),1);
    }
    #[tokio::test] async fn atomic_rename_retries_eexist_after_unlink() {
        struct Operations {renames:AtomicU64,unlinks:AtomicU64}
        impl crate::types::AtomicWriteOperations for Operations {
            fn write_file<'a>(&'a self,path:&'a Path,content:&'a [u8])->crate::types::AtomicWriteFuture<'a> { FileOperations.write_file(path,content) }
            fn rename<'a>(&'a self,from:&'a Path,to:&'a Path)->crate::types::AtomicWriteFuture<'a> { Box::pin(async move { if self.renames.fetch_add(1,Ordering::Relaxed)==0 { return Err(std::io::ErrorKind::AlreadyExists.into()); } tokio::fs::rename(from,to).await }) }
            fn unlink<'a>(&'a self,path:&'a Path)->crate::types::AtomicWriteFuture<'a> { self.unlinks.fetch_add(1,Ordering::Relaxed); FileOperations.unlink(path) }
        }
        let directory=tempfile::tempdir().unwrap(); let path=directory.path().join("atomic.txt"); tokio::fs::write(&path,b"old\n").await.unwrap(); let operations=Operations{renames:AtomicU64::new(0),unlinks:AtomicU64::new(0)};
        write_file_atomic_with_operations(&path,b"new\n",&operations).await.unwrap(); assert_eq!(tokio::fs::read(&path).await.unwrap(),b"new\n"); assert_eq!(operations.renames.load(Ordering::Relaxed),2); assert_eq!(operations.unlinks.load(Ordering::Relaxed),1); assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(),1);
    }
    #[tokio::test] async fn detailed_result_tracks_fuzz_tier() { let directory=tempfile::tempdir().unwrap(); tokio::fs::write(directory.path().join("fuzz.txt"),"value   \n").await.unwrap(); let result=apply_patch_detailed(directory.path(),"*** Begin Patch\n*** Update File: fuzz.txt\n@@\n-value\n+value!\n*** End Patch").await.unwrap(); assert!(result.failures.is_empty()); assert!(result.details.fuzz>0); }
    #[cfg(unix)]
    #[tokio::test] async fn directory_symlink_can_target_outside_workspace() { let workspace=tempfile::tempdir().unwrap(); let outside=tempfile::tempdir().unwrap(); std::os::unix::fs::symlink(outside.path(),workspace.path().join("link")).unwrap(); apply_patch(workspace.path(),"*** Begin Patch\n*** Add File: link/outside.txt\n+outside\n*** End Patch").await.unwrap(); assert_eq!(tokio::fs::read_to_string(outside.path().join("outside.txt")).await.unwrap(),"outside\n"); }
    #[tokio::test] async fn directory_failure_is_not_a_context_reread_candidate() { let directory=tempfile::tempdir().unwrap(); tokio::fs::create_dir(directory.path().join("folder")).await.unwrap(); let result=apply_patch_detailed(directory.path(),"*** Begin Patch\n*** Delete File: folder\n*** End Patch").await.unwrap(); assert_eq!(result.failures[0].code.as_deref(),Some("EISDIR")); assert!(result.recovery_instructions.must_read_files.is_empty()); }
    #[tokio::test] async fn progress_errors_do_not_interrupt_mutations() {
        let directory=tempfile::tempdir().unwrap(); tokio::fs::write(directory.path().join("first.txt"),"one\n").await.unwrap(); tokio::fs::write(directory.path().join("second.txt"),"two\n").await.unwrap();
        let callback=|_|Box::pin(async { Err("render failed".into()) }) as std::pin::Pin<Box<dyn std::future::Future<Output=Result<(),String>>+Send>>;
        let result=apply_patch_detailed_with_progress(directory.path(),"*** Begin Patch\n*** Update File: first.txt\n@@\n-one\n+ONE\n*** Update File: second.txt\n@@\n-two\n+TWO\n*** End Patch",Some(&callback)).await.unwrap();
        assert!(result.failures.is_empty()); assert_eq!(result.applied_files,["first.txt","second.txt"]); assert_eq!(tokio::fs::read_to_string(directory.path().join("first.txt")).await.unwrap(),"ONE\n"); assert_eq!(tokio::fs::read_to_string(directory.path().join("second.txt")).await.unwrap(),"TWO\n");
    }
    #[tokio::test] async fn concurrent_patches_to_same_file_preserve_both_updates() {
        let directory=tempfile::tempdir().unwrap(); let path=directory.path().join("shared.txt"); tokio::fs::write(&path,"first\nsecond\n").await.unwrap();
        let first=apply_patch_detailed(directory.path(),"*** Begin Patch\n*** Update File: shared.txt\n@@\n-first\n+FIRST\n*** End Patch");
        let second=apply_patch_detailed(directory.path(),"*** Begin Patch\n*** Update File: shared.txt\n@@\n-second\n+SECOND\n*** End Patch");
        let (first,second)=tokio::join!(first,second); assert!(first.unwrap().failures.is_empty()); assert!(second.unwrap().failures.is_empty()); assert_eq!(tokio::fs::read_to_string(path).await.unwrap(),"FIRST\nSECOND\n");
    }
    #[tokio::test] async fn detailed_application_continues_after_failure() {
        let directory=tempfile::tempdir().unwrap();
        let patch="*** Begin Patch\n*** Add File: first\n+one\n*** Update File: missing\n@@\n-old\n+new\n*** Add File: last\n+three\n*** End Patch";
        let result=apply_patch_detailed(directory.path(),patch).await.unwrap();
        assert_eq!(result.applied_files,["first","last"]); assert_eq!(result.failures.len(),1); assert_eq!(result.failures[0].code.as_deref(),Some("ENOENT")); assert!(result.has_partial_success);
        assert_eq!(tokio::fs::read_to_string(directory.path().join("last")).await.unwrap(),"three\n");
    }
    #[tokio::test] async fn fail_fast_keeps_earlier_success_and_does_not_apply_later_files() {
        let directory=tempfile::tempdir().unwrap();
        let patch="*** Begin Patch\n*** Add File: first\n+one\n*** Delete File: missing\n*** Add File: last\n+three\n*** End Patch";
        let error=apply_patch(directory.path(),patch).await.unwrap_err();
        assert_eq!(error.result.applied_files,["first"]); assert!(error.has_partial_success()); assert!(!directory.path().join("last").exists()); assert_eq!(error.result.details.applied_operations[0].preview.diff,"");
    }
    #[tokio::test] async fn text_move_and_delete_use_real_filesystem() {
        let directory=tempfile::tempdir().unwrap();
        apply_patch(directory.path(),"*** Begin Patch\n*** Add File: a\n+old\n*** End Patch").await.unwrap();
        apply_patch(directory.path(),"*** Begin Patch\n*** Update File: a\n*** Move to: nested/b\n@@\n-old\n+new\n*** End Patch").await.unwrap();
        assert!(!directory.path().join("a").exists()); assert_eq!(tokio::fs::read_to_string(directory.path().join("nested/b")).await.unwrap(),"new\n");
        apply_patch(directory.path(),"*** Begin Patch\n*** Delete File: nested/b\n*** End Patch").await.unwrap(); assert!(!directory.path().join("nested/b").exists());
    }
    #[tokio::test] async fn binary_move_preserves_exact_bytes() {
        let directory=tempfile::tempdir().unwrap(); let bytes=[0,255,1,2]; tokio::fs::write(directory.path().join("a"),bytes).await.unwrap();
        let result=apply_patch_detailed(directory.path(),"*** Begin Patch\n*** Update File: a\n*** Move to: b\n*** End Patch").await.unwrap(); assert!(result.failures.is_empty()); assert_eq!(tokio::fs::read(directory.path().join("b")).await.unwrap(),bytes); assert!(!directory.path().join("a").exists());
    }
    #[test] fn empty_envelope_is_rejected_before_any_mutation() { assert_eq!(parse_non_empty_patch("*** Begin Patch\n*** End Patch").unwrap_err(),"patch rejected: empty patch"); assert_eq!(parse_non_empty_patch("*** Begin Patch\nnoise\n*** End Patch").unwrap_err(),"apply_patch verification failed: no hunks found"); }
    #[test] fn compact_result_preserves_recovery_metadata() { let mut result=ApplyPatchResult::default(); result.applied_files.push("a".into()); result.details.applied_operations.push(AppliedPatchOperation{operation_index:1,preview:ApplyPatchPreviewFile{file_path:"a".into(),move_path:None,operation:ApplyPatchOperation::Add,binary:None,diff:"large".into(),patch:Some("large".into()),added:1,removed:0}}); let compact=compact_apply_patch_result(result); assert_eq!(compact.applied_files,["a"]); assert_eq!(compact.details.applied_operations[0].operation_index,1); assert!(compact.details.applied_operations[0].preview.diff.is_empty()); assert_eq!(compact.details.applied_operations[0].preview.patch,None); }
}
