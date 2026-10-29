use std::path::Path;
pub const PRUNE_TOMBSTONE_PREFIX:&str=".prune-";
const DISPOSABLE:[&str;2]=["facts-payload.json",".sandbox-tmp"];
pub fn remove_run_artifact(path:&Path)->std::io::Result<()> {
    let metadata=match std::fs::symlink_metadata(path){Ok(metadata)=>metadata,Err(error) if error.kind()==std::io::ErrorKind::NotFound=>return Ok(()),Err(error)=>return Err(error)};
    let result=if metadata.is_dir(){std::fs::remove_dir_all(path)}else{std::fs::remove_file(path)};
    match result{Err(error) if error.kind()==std::io::ErrorKind::NotFound=>Ok(()),result=>result}
}
pub fn is_terminal_run_dir(dir:&Path)->bool{dir.join("final.json").exists()||dir.join("abandoned.json").exists()}
/// Caller must publish its terminal sentinel before removing reconciliation inputs.
pub fn cleanup_terminal_facts_run(dir:&Path,remove:&mut dyn FnMut(&Path)->std::io::Result<()>,warn:&mut dyn FnMut(&str,&Path,&std::io::Error)) {
    for artifact in DISPOSABLE{let path=dir.join(artifact);if let Err(error)=remove(&path)&&error.kind()!=std::io::ErrorKind::NotFound{warn("facts run artifact cleanup failed",&path,&error);}}
}
pub fn sweep_terminal_facts_runs(facts_dir:&Path,remove:&mut dyn FnMut(&Path)->std::io::Result<()>,warn:&mut dyn FnMut(&str,&Path,&std::io::Error)){
    let runs=facts_dir.join("runs");let Ok(entries)=std::fs::read_dir(&runs)else{return;};let mut names:Vec<_>=entries.filter_map(Result::ok).map(|entry|entry.file_name()).collect();names.sort();
    for name in names{
        let dir=runs.join(&name);
        if name.to_string_lossy().starts_with(PRUNE_TOMBSTONE_PREFIX){
            if let Err(error)=remove(&dir){warn("facts run tombstone cleanup failed",&dir,&error);}
            continue;
        }
        if !is_terminal_run_dir(&dir)||!DISPOSABLE.iter().any(|artifact|dir.join(artifact).exists()){continue;}
        cleanup_terminal_facts_run(&dir,remove,warn);
    }
}
#[cfg(test)]
mod tests{
    use super::*;
    fn seed(facts:&Path,name:&str,sentinel:Option<&str>)->std::path::PathBuf{let dir=facts.join("runs").join(name);std::fs::create_dir_all(dir.join(".sandbox-tmp")).unwrap();for name in ["facts-payload.json","ledger.json","outcome.json","extraction.jsonl","child-stderr.log"]{std::fs::write(dir.join(name),"{}").unwrap();}if let Some(sentinel)=sentinel{std::fs::write(dir.join(sentinel),"{}").unwrap();}dir}
    #[test]fn terminal_sweep_retains_diagnostics_and_live_payload(){let root=tempfile::tempdir().unwrap();let final_dir=seed(root.path(),"final",Some("final.json"));let abandoned=seed(root.path(),"abandoned",Some("abandoned.json"));let live=seed(root.path(),"live",None);let tombstone=seed(root.path(),".prune-old",None);sweep_terminal_facts_runs(root.path(),&mut remove_run_artifact,&mut |_,_,error|panic!("{error}"));for dir in [final_dir,abandoned]{assert!(is_terminal_run_dir(&dir));assert!(!dir.join("facts-payload.json").exists());assert!(!dir.join(".sandbox-tmp").exists());for artifact in ["ledger.json","outcome.json","extraction.jsonl","child-stderr.log"]{assert!(dir.join(artifact).exists());}}assert!(live.join("facts-payload.json").exists());assert!(!tombstone.exists());}
    #[test]fn removal_errors_warn_and_do_not_rewrite_sentinel(){let root=tempfile::tempdir().unwrap();let dir=seed(root.path(),"final",Some("final.json"));let mut warned=0;cleanup_terminal_facts_run(&dir,&mut |_|Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied)),&mut |_,_,_|warned+=1);assert_eq!(warned,2);assert_eq!(std::fs::read(dir.join("final.json")).unwrap(),b"{}");assert!(dir.join("facts-payload.json").exists());}
    #[test]fn missing_artifacts_do_not_warn(){let root=tempfile::tempdir().unwrap();cleanup_terminal_facts_run(root.path(),&mut remove_run_artifact,&mut |_,_,_|panic!("warn"));sweep_terminal_facts_runs(root.path(),&mut remove_run_artifact,&mut |_,_,_|panic!("warn"));}
    #[cfg(unix)]
    #[test]fn deletion_does_not_follow_symlink(){let root=tempfile::tempdir().unwrap();let real=root.path().join("real");std::fs::create_dir(&real).unwrap();std::fs::write(real.join("keep"),"data").unwrap();let alias=root.path().join("alias");std::os::unix::fs::symlink(&real,&alias).unwrap();remove_run_artifact(&alias).unwrap();assert!(real.join("keep").exists());assert!(!alias.exists());}
}
