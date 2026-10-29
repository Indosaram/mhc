use std::path::{Path,PathBuf};
use memory_core::identity::layout::{AGENTS_DIRNAME,REPO_DIRNAME,MemoryIdentityPaths,build_identity_paths};

pub const TRANSIENT_DIRNAME:&str="transient-runs";
pub struct IdentityRunPaths { pub paths:MemoryIdentityPaths,pub durable_root:PathBuf,pub transient_run_root:Option<PathBuf> }
pub enum IdentityRunDisposition { Durable,Removed,Kept }

pub fn is_durable_identity_root(root:&Path)->bool{root.join(REPO_DIRNAME).exists()}
pub fn resolve_identity_run_paths(identity:&str,identity_paths:MemoryIdentityPaths,memory_root:&Path,one_shot:bool,token:Option<&str>)->IdentityRunPaths{
    let durable_root=identity_paths.root.clone();
    if !one_shot||is_durable_identity_root(&durable_root){return IdentityRunPaths{paths:identity_paths,durable_root,transient_run_root:None};}
    let run_root=memory_root.join(TRANSIENT_DIRNAME).join(token.unwrap_or("run"));
    IdentityRunPaths{paths:build_identity_paths(&run_root,identity),durable_root,transient_run_root:Some(run_root)}
}
pub fn finalize_identity_run(run:&IdentityRunPaths)->std::io::Result<IdentityRunDisposition>{
    let Some(root)=&run.transient_run_root else{return Ok(IdentityRunDisposition::Durable)};
    if is_durable_identity_root(&run.paths.root){return Ok(IdentityRunDisposition::Kept);}
    match std::fs::remove_dir_all(root){Ok(())=>Ok(IdentityRunDisposition::Removed),Err(error) if error.kind()==std::io::ErrorKind::NotFound=>Ok(IdentityRunDisposition::Removed),Err(error)=>Err(error)}
}
pub fn transient_run_root_from_identity(root:&Path)->Option<PathBuf>{root.ancestors().nth(2).map(Path::to_path_buf)}
pub fn transient_run_root(memory_root:&Path,token:&str)->PathBuf{memory_root.join(TRANSIENT_DIRNAME).join(token)}
pub fn transient_identity_root(run_root:&Path,identity:&str)->PathBuf{run_root.join(AGENTS_DIRNAME).join(identity)}

pub fn sweep_transient_run_roots(memory_root:&Path,now_ms:i64,max_age_ms:i64)->std::io::Result<(usize,usize)>{
    let area=memory_root.join(TRANSIENT_DIRNAME);let mut removed=0;let mut promoted=0;
    let entries=match std::fs::read_dir(&area){Ok(entries)=>entries,Err(error)if error.kind()==std::io::ErrorKind::NotFound=>return Ok((0,0)),Err(error)=>return Err(error)};
    for entry in entries.flatten(){let token=entry.file_name();if !token.to_string_lossy().chars().all(|ch|ch.is_ascii_alphanumeric()||ch=='-'){continue;}let token_text=token.to_string_lossy();let Some(pid)=token_text.split('-').nth(1).and_then(|pid|pid.parse::<u32>().ok())else{continue;};if process_is_alive(pid){continue;}let root=entry.path();
        let modified=std::fs::metadata(&root).and_then(|meta|meta.modified()).ok().and_then(|time|time.duration_since(std::time::UNIX_EPOCH).ok()).map(|duration|duration.as_millis() as i64);
        if modified.is_none_or(|at|now_ms.saturating_sub(at)<max_age_ms){continue;}
        let identities=root.join(AGENTS_DIRNAME);let children=match std::fs::read_dir(&identities){Ok(children)=>children,Err(_)=>{std::fs::remove_dir_all(&root)?;removed+=1;continue;}};
        let mut stranded=false;
        for child in children.flatten(){let source=child.path();if !is_durable_identity_root(&source){continue;}let target=memory_root.join(AGENTS_DIRNAME).join(child.file_name());if target.exists(){stranded=true;continue;}std::fs::create_dir_all(target.parent().unwrap_or(memory_root))?;match std::fs::rename(source,target){Ok(())=>promoted+=1,Err(_)=>stranded=true}}
        if !stranded{std::fs::remove_dir_all(&root)?;removed+=1;}else{let report=root.join(format!("stranded-{}.json",std::process::id()));let _=std::fs::write(report,"{}\n");}
    }
    Ok((removed,promoted))
}
fn process_is_alive(pid:u32)->bool{memory_core::locks::get_pid_liveness(pid)==memory_core::locks::ProcessLiveness::Alive}

pub fn sweep_abandoned_identity_roots(memory_root:&Path,now_ms:i64,max_age_ms:i64)->std::io::Result<usize>{
    let agents=memory_root.join(AGENTS_DIRNAME);let entries=match std::fs::read_dir(&agents){Ok(entries)=>entries,Err(error)if error.kind()==std::io::ErrorKind::NotFound=>return Ok(0),Err(error)=>return Err(error)};let mut removed=0;
    for entry in entries.flatten(){let root=entry.path();if is_durable_identity_root(&root){continue;}let modified=std::fs::metadata(&root).and_then(|meta|meta.modified()).ok().and_then(|time|time.duration_since(std::time::UNIX_EPOCH).ok()).map(|duration|duration.as_millis() as i64);if modified.is_none_or(|at|now_ms.saturating_sub(at)<max_age_ms){continue;}std::fs::remove_dir_all(root)?;removed+=1;}
    Ok(removed)
}

#[cfg(test)]mod tests{
    use super::*;
    #[test]fn one_shot_empty_identity_is_removed_but_persisted_repo_is_kept(){let root=tempfile::tempdir().unwrap();let paths=build_identity_paths(root.path(),"agent");let run=resolve_identity_run_paths("agent",paths,root.path(),true,Some("token"));std::fs::create_dir_all(&run.paths.runtime).unwrap();assert!(matches!(finalize_identity_run(&run).unwrap(),IdentityRunDisposition::Removed));let paths=build_identity_paths(root.path(),"agent");let run=resolve_identity_run_paths("agent",paths,root.path(),true,Some("token-2"));std::fs::create_dir_all(&run.paths.repo).unwrap();assert!(matches!(finalize_identity_run(&run).unwrap(),IdentityRunDisposition::Kept));}
}
