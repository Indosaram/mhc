use std::path::{Component, Path, PathBuf};
use crate::context::MemoryIdentityContext;

#[derive(Debug, Clone, Copy)]
pub enum FilesystemOperation { Read, Enumerate, Write }
impl FilesystemOperation { fn name(self)->&'static str {match self {Self::Read=>"read",Self::Enumerate=>"enumerate",Self::Write=>"write"}} }
#[derive(Debug, PartialEq, Eq)]
pub enum FilesystemPolicyDecision { Allow, Deny {reason:String} }
pub struct FilesystemPolicy { pub denied_roots:Vec<PathBuf>,own_roots:Vec<PathBuf>,agents_roots:Vec<PathBuf> }
pub trait FilesystemPolicyRegistration {fn register_filesystem_policy(&mut self,policy:FilesystemPolicy);}
#[derive(Debug,PartialEq,Eq)]
pub enum MemoryPolicyRegistration {Registered,Unbound,Unsupported}
pub fn register_memory_filesystem_policy(api:Option<&mut dyn FilesystemPolicyRegistration>,context:Option<&MemoryIdentityContext>)->std::io::Result<MemoryPolicyRegistration>{let Some(context)=context else{return Ok(MemoryPolicyRegistration::Unbound);};let Some(api)=api else{return Ok(MemoryPolicyRegistration::Unsupported);};api.register_filesystem_policy(build_memory_filesystem_policy(context)?);Ok(MemoryPolicyRegistration::Registered)}
pub fn build_memory_filesystem_policy(context:&MemoryIdentityContext)->std::io::Result<FilesystemPolicy>{
    let own=resolve_path(&context.identity_paths.root)?;let agents=own.parent().unwrap_or(Path::new("/")).to_path_buf();
    let mut foreign=Vec::new();match std::fs::read_dir(&agents){Ok(entries)=>{for entry in entries{let entry=entry?;if entry.file_name()!=std::ffi::OsStr::new(&context.identity){foreign.push(entry.path());}}},Err(error) if missing(&error)=>{},Err(error)=>return Err(error)}
    Ok(FilesystemPolicy{denied_roots:stable_roots(&foreign),own_roots:stable_roots(&[own]),agents_roots:stable_roots(&[agents])})
}
impl FilesystemPolicy {
    pub fn check(&self,operation:FilesystemOperation,path:&Path)->std::io::Result<FilesystemPolicyDecision>{
        let target=resolve_path(path)?;
        if self.own_roots.iter().any(|root|within(root,&target)) {
            return Ok(FilesystemPolicyDecision::Allow);
        }
        if self.agents_roots.iter().any(|root|within(root,&target)) {
            return Ok(FilesystemPolicyDecision::Deny{reason:format!("cross-identity memory access denied: {} {}",operation.name(),path.display())});
        }
        Ok(FilesystemPolicyDecision::Allow)
    }
}
pub(crate) fn resolve_path(path:&Path)->std::io::Result<PathBuf>{let absolute=if path.is_absolute(){path.to_path_buf()}else{std::env::current_dir()?.join(path)};let mut result=PathBuf::new();for part in absolute.components(){match part{Component::CurDir=>{},Component::ParentDir=>{result.pop();},other=>result.push(other.as_os_str())}}Ok(result)}
fn missing(error:&std::io::Error)->bool{matches!(error.kind(),std::io::ErrorKind::NotFound|std::io::ErrorKind::NotADirectory)}
pub(crate) fn canonicalize_from_nearest_existing(target:&Path)->Option<PathBuf>{let mut candidate=target.to_path_buf();let mut segments=Vec::new();loop{match std::fs::symlink_metadata(&candidate){Ok(_)=>{let mut result=std::fs::canonicalize(&candidate).ok()?;for segment in segments.iter().rev(){result.push(segment);}return resolve_path(&result).ok();},Err(error) if missing(&error)=>{segments.push(candidate.file_name()?.to_os_string());candidate=candidate.parent()?.to_path_buf();},Err(_)=>return None}}}
pub(crate) fn stable_roots(paths:&[PathBuf])->Vec<PathBuf>{let mut roots=Vec::new();for path in paths{for root in [Some(path.clone()),canonicalize_from_nearest_existing(path)].into_iter().flatten(){if !roots.contains(&root){roots.push(root);}}}roots}
pub(crate) fn within(root:&Path,target:&Path)->bool{let Ok(relative)=target.strip_prefix(root)else{return false;};!relative.to_string_lossy().starts_with("..")}
#[cfg(test)]
mod tests {
    use super::*;
    use memory_core::identity::layout::build_identity_paths;
    fn context(root:&Path)->MemoryIdentityContext{let paths=build_identity_paths(root,"own");MemoryIdentityContext::new("own".into(),paths,crate::binding::MemorySessionBinding{identity:"own".into(),repo_path_hash:"hash".into(),bound_at:1.0})}
    #[test]fn foreign_and_new_sibling_denied_own_and_outside_allowed(){let dir=tempfile::tempdir().unwrap();let context=context(dir.path());let agents=context.identity_paths.root.parent().unwrap();std::fs::create_dir_all(agents.join("foreign")).unwrap();let policy=build_memory_filesystem_policy(&context).unwrap();assert!(policy.denied_roots.contains(&agents.join("foreign")));assert!(!policy.denied_roots.contains(&context.identity_paths.root));for operation in [FilesystemOperation::Read,FilesystemOperation::Write,FilesystemOperation::Enumerate]{for path in [agents.to_path_buf(),agents.join("foreign/repo/a"),agents.join("new/repo/a")]{assert!(matches!(policy.check(operation,&path).unwrap(),FilesystemPolicyDecision::Deny{..}));}for path in [context.identity_paths.repo.clone(),context.identity_paths.worktrees.clone(),dir.path().join("outside")]{assert_eq!(policy.check(operation,&path).unwrap(),FilesystemPolicyDecision::Allow);}}}
    #[test]fn registration_unbound_and_unsupported(){assert_eq!(register_memory_filesystem_policy(None,None).unwrap(),MemoryPolicyRegistration::Unbound);let dir=tempfile::tempdir().unwrap();assert_eq!(register_memory_filesystem_policy(None,Some(&context(dir.path()))).unwrap(),MemoryPolicyRegistration::Unsupported);}
    #[cfg(unix)]
    #[test]fn symlinked_missing_identity_roots_match_canonical_targets(){let dir=tempfile::tempdir().unwrap();let real=dir.path().join("real");std::fs::create_dir_all(&real).unwrap();let alias=dir.path().join("alias");std::os::unix::fs::symlink(&real,&alias).unwrap();let context=context(&alias);let policy=build_memory_filesystem_policy(&context).unwrap();let own=context.identity_paths.root.strip_prefix(&alias).unwrap();assert_eq!(policy.check(FilesystemOperation::Read,&real.join(own).join("repo/new.md")).unwrap(),FilesystemPolicyDecision::Allow);let agents=real.join(own.parent().unwrap());assert!(matches!(policy.check(FilesystemOperation::Read,&agents.join("foreign/repo/new.md")).unwrap(),FilesystemPolicyDecision::Deny{..}));}
}
