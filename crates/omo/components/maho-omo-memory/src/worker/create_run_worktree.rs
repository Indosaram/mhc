use std::path::Path;
use memory_core::{git::GitMemoryRepo,identity::layout::MemoryIdentityPaths,reflection::worktree::{ReflectionWorktree,ReflectionWorktreeIdentity}};
pub fn create_run_worktree(repo:&GitMemoryRepo,run_id:&str,paths:&MemoryIdentityPaths,create:impl FnOnce(&GitMemoryRepo,&str,&Path,&dyn Fn(&ReflectionWorktreeIdentity)->Result<(),String>)->Result<ReflectionWorktree,String>)->Result<ReflectionWorktree,String> {
    let dir=paths.reflection.join("runs").join(run_id);
    let result=create(repo,run_id,&paths.worktrees,&|identity| {
        let mut builder=std::fs::DirBuilder::new();builder.recursive(true);
        #[cfg(unix)] {use std::os::unix::fs::DirBuilderExt;builder.mode(0o700);}
        builder.create(&dir).map_err(|error|error.to_string())?;
        super::run_artifacts::write_run_json_atomic(&dir.join("prelaunch.json"),&super::run_artifacts::RunPrelaunchArtifact{version:1,run_id:run_id.into(),worktree_dir:identity.dir.to_string_lossy().into_owned(),worktree_branch:identity.branch.clone()},0o600).map_err(|error|error.to_string())
    });
    if result.is_err() {
        match std::fs::remove_dir_all(&dir){Ok(())=>{},Err(error)if error.kind()==std::io::ErrorKind::NotFound=>{},Err(error)=>return Err(error.to_string())}
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prelaunch_is_durable_before_creation_and_removed_on_failure() {
        let root=tempfile::tempdir().unwrap();let paths=memory_core::identity::layout::build_identity_paths(root.path(),"agent");let repo=GitMemoryRepo::open(&paths.repo,"agent").unwrap();
        let result=create_run_worktree(&repo,"run",&paths,|_,id,worktrees,before| {
            assert_eq!(id,"run");assert_eq!(worktrees,paths.worktrees);
            before(&ReflectionWorktreeIdentity{dir:worktrees.join("run"),branch:"memory/run".into()})?;
            let value:super::super::run_artifacts::RunPrelaunchArtifact=super::super::run_artifacts::read_run_json(&paths.reflection.join("runs/run/prelaunch.json")).unwrap();assert_eq!(value.run_id,"run");assert_eq!(value.worktree_branch,"memory/run");
            Err("creation failed".into())
        });
        assert_eq!(result.err().as_deref(),Some("creation failed"));assert!(!paths.reflection.join("runs/run").exists());
    }
}
