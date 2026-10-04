use std::{path::PathBuf,sync::Arc};
use memory_core::{git::{GitMemoryRepo,errors::GitError,repo_types::{GitCommitAuthor,GitSeedFile,InitializeGitRepoOptions}},identity::layout::MemoryIdentityPaths,locks::{AcquireLockOptions,CreateLockRecordOptions,WithLockError,create_lock_record,memory_writer_lock_path,with_lock},tools::{memory::{MemoryToolLock,MemoryToolResult},tool_errors::MemoryToolError}};
pub struct MemoryEngineSession {pub repo:GitMemoryRepo,pub lock:MemoryWriterLock,pub author:GitCommitAuthor}
#[derive(Default)]
pub struct MemoryEngineSessionOptions {pub lock_wait_timeout_ms:Option<u64>,pub lock_retry_delay_ms:Option<u64>}
pub struct MemoryWriterLock{identity:String,path:PathBuf,wait_timeout_ms:u64,retry_delay_ms:Option<u64>}
impl MemoryWriterLock {
    pub fn run<T,E:std::fmt::Display>(&self,domain:&str,operation:impl FnOnce()->Result<T,E>)->Result<T,MemoryToolError>{
        if domain!="memory-write"{return Err(MemoryToolError::new(format!("unsupported lock domain '{domain}'")));}
        let record=create_lock_record(&format!("memory tool ({})",self.identity),CreateLockRecordOptions::default()).map_err(|error|MemoryToolError::new(error.to_string()))?;
        with_lock(&self.path,&record,&AcquireLockOptions{wait_timeout_ms:Some(self.wait_timeout_ms),retry_delay_ms:self.retry_delay_ms,..Default::default()},operation).map_err(|error|match error{WithLockError::User(error)=>MemoryToolError::new(error.to_string()),WithLockError::Acquire(error)=>MemoryToolError::new(error.to_string())})
    }
}
impl MemoryToolLock for MemoryWriterLock{fn with_lock(&self,domain:&str,operation:&mut dyn FnMut()->Result<MemoryToolResult,MemoryToolError>)->Result<MemoryToolResult,MemoryToolError>{self.run(domain,operation)}}
pub fn prepare_memory_engine_session(identity:&str,paths:&MemoryIdentityPaths,options:MemoryEngineSessionOptions)->Result<MemoryEngineSession,MemoryToolError>{
    crate::context::ensure_identity_runtime_dirs(paths).map_err(|error|MemoryToolError::new(error.to_string()))?;
    let repo=GitMemoryRepo::open(&paths.repo,identity).map_err(|error|MemoryToolError::new(error.to_string()))?;
    let lock=MemoryWriterLock{identity:identity.into(),path:memory_writer_lock_path(&paths.locks),wait_timeout_ms:options.lock_wait_timeout_ms.unwrap_or(5000),retry_delay_ms:options.lock_retry_delay_ms};
    if !paths.repo.join(".git").exists(){lock.run("memory-write",||{if !paths.repo.join(".git").exists(){repo.init(InitializeGitRepoOptions{seed_files:memory_core::seeds::seeds::build_default_seed_files().into_iter().map(|seed|GitSeedFile{relative_path:seed.relative_path,content:seed.content}).collect(),install_hooks:Some(Arc::new(|dir|memory_core::memfs::install_hooks(dir).map(|_|()).map_err(GitError::Io))),..Default::default()})?;}Ok::<_,GitError>(())})?;}
    Ok(MemoryEngineSession{repo,lock,author:GitCommitAuthor{agent_id:identity.into(),author_name:identity.into(),author_email:None}})
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]fn first_write_seeds_real_repo_and_preserves_existing_head(){let dir=tempfile::tempdir().unwrap();let paths=memory_core::identity::layout::build_identity_paths(dir.path(),"agent");assert!(!paths.repo.exists());let session=prepare_memory_engine_session("agent",&paths,Default::default()).unwrap();assert!(paths.repo.join("system/persona.md").exists());assert!(paths.repo.join("system/human.md").exists());assert!(paths.repo.join(".git/hooks/pre-commit").exists());let head=session.repo.head().unwrap().unwrap();let second=prepare_memory_engine_session("agent",&paths,Default::default()).unwrap();assert_eq!(second.repo.head().unwrap(),Some(head));assert!(!memory_writer_lock_path(&paths.locks).exists());assert_eq!(session.author.agent_id,"agent");}
    #[test]fn unsupported_domain_does_not_run_mutation(){let dir=tempfile::tempdir().unwrap();let lock=MemoryWriterLock{identity:"agent".into(),path:dir.path().join("writer.lock"),wait_timeout_ms:0,retry_delay_ms:None};assert!(lock.run::<(),MemoryToolError>("other",||panic!("operation")).is_err());assert!(!lock.path.exists());}
    #[test]fn mutation_error_releases_lock(){let dir=tempfile::tempdir().unwrap();let lock=MemoryWriterLock{identity:"agent".into(),path:dir.path().join("writer.lock"),wait_timeout_ms:0,retry_delay_ms:None};assert_eq!(lock.run::<(),_>("memory-write",||Err("operation failed")).unwrap_err().message,"operation failed");assert!(!lock.path.exists());}
}
