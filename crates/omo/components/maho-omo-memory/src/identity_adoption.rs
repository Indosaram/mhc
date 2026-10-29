use std::path::Path;
use memory_core::identity::{layout::{MemoryIdentityPaths,build_identity_paths},resolve::is_auto_agent_value};
use crate::binding::{MemorySessionBinding,create_memory_binding};

pub struct BoundIdentity { pub id: String, pub paths: MemoryIdentityPaths }
pub enum IdentityAdoption { Resolved(BoundIdentity), Rebound(BoundIdentity), Conflict }

pub fn adopt_session_identity(recorded: Option<&MemorySessionBinding>, resolved: BoundIdentity, memory_root: &Path, config_agent_value: Option<&str>, verify_repository: bool) -> IdentityAdoption {
    let Some(recorded)=recorded else{return IdentityAdoption::Resolved(resolved)};
    if recorded.identity==resolved.id {
        return if (verify_repository||!recorded.repo_path_hash.is_empty())&&!repository_matches(recorded,&resolved){IdentityAdoption::Conflict}else{IdentityAdoption::Resolved(resolved)};
    }
    if !is_auto_agent_value(config_agent_value){return IdentityAdoption::Conflict;}
    let adopted=BoundIdentity{id:recorded.identity.clone(),paths:build_identity_paths(memory_root,&recorded.identity)};
    if repository_matches(recorded,&adopted){IdentityAdoption::Rebound(adopted)}else{IdentityAdoption::Conflict}
}

fn repository_matches(recorded:&MemorySessionBinding,identity:&BoundIdentity)->bool {
    let rebuilt=create_memory_binding(&identity.id,&identity.paths.repo.to_string_lossy(),recorded.bound_at);
    rebuilt.repo_path_hash==recorded.repo_path_hash
}

#[cfg(test)]mod tests{
    use super::*;
    #[test]fn automatic_config_adopts_recorded_identity(){let root=tempfile::tempdir().unwrap();let prior=build_identity_paths(root.path(),"prior");let recorded=create_memory_binding("prior",&prior.repo.to_string_lossy(),1.0);let current=BoundIdentity{id:"current".into(),paths:build_identity_paths(root.path(),"current")};assert!(matches!(adopt_session_identity(Some(&recorded),current,root.path(),None,false),IdentityAdoption::Rebound(_)));}
    #[test]fn explicit_identity_change_conflicts(){let root=tempfile::tempdir().unwrap();let prior=build_identity_paths(root.path(),"prior");let recorded=create_memory_binding("prior",&prior.repo.to_string_lossy(),1.0);let current=BoundIdentity{id:"current".into(),paths:build_identity_paths(root.path(),"current")};assert!(matches!(adopt_session_identity(Some(&recorded),current,root.path(),Some("current"),false),IdentityAdoption::Conflict));}
}
