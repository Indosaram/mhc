use std::{collections::BTreeMap,path::{Path,PathBuf},sync::Arc};
pub struct RpcSessionLaunchProfile{pub runtime:maho_core::agent_session_runtime::AgentSessionLaunchProfile,pub session_path:Option<String>,pub durable_session_id:Option<String>,pub session_kind:Option<maho_ext_api::SessionKind>,pub session_context:Option<BTreeMap<String,String>>}
pub struct SessionIdentity{pub kind:maho_ext_api::SessionKind,pub context:Arc<BTreeMap<String,String>>}
pub fn session_identity(profile:&RpcSessionLaunchProfile)->SessionIdentity{SessionIdentity{kind:profile.session_kind.unwrap_or_default(),context:Arc::new(profile.session_context.clone().unwrap_or_default())}}
pub fn canonical_path(path:&Path)->std::io::Result<PathBuf>{if path.exists(){path.canonicalize()}else{let parent=path.parent().unwrap_or(Path::new("."));Ok(parent.canonicalize()?.join(path.file_name().unwrap_or_default()))}}
#[derive(Debug,thiserror::Error,PartialEq,Eq)]#[error("{code}{suffix}")]
pub struct RpcSessionRegistryError{pub code:String,pub detail:Option<serde_json::Value>,suffix:String}
impl RpcSessionRegistryError{pub fn new(code:&str,reason:Option<&str>,detail:Option<serde_json::Value>)->Self{Self{code:code.into(),detail,suffix:if code=="open_failed"{reason.filter(|reason|!reason.is_empty()).map_or(String::new(),|reason|format!(": {reason}"))}else{String::new()}}}}
#[cfg(test)]mod tests{
    use super::*;
    #[test]fn identity_defaults_and_context_do_not_alias_client_inputs(){let mut profile=RpcSessionLaunchProfile{runtime:Default::default(),session_path:None,durable_session_id:None,session_kind:None,session_context:Some(BTreeMap::from([("owner".into(),"a".into())]))};let identity=session_identity(&profile);profile.session_context.as_mut().unwrap().insert("owner".into(),"b".into());assert_eq!(identity.kind,maho_ext_api::SessionKind::Interactive);assert_eq!(identity.context["owner"],"a");}
    #[test]fn canonical_missing_leaf_uses_real_parent(){let temp=tempfile::tempdir().unwrap();let parent=temp.path().join("real");std::fs::create_dir(&parent).unwrap();#[cfg(unix)]{let alias=temp.path().join("alias");std::os::unix::fs::symlink(&parent,&alias).unwrap();assert_eq!(canonical_path(&alias.join("missing.jsonl")).unwrap(),parent.join("missing.jsonl"));}}
    #[test]fn only_open_failed_appends_reason(){assert_eq!(RpcSessionRegistryError::new("open_failed",Some("bad"),None).to_string(),"open_failed: bad");assert_eq!(RpcSessionRegistryError::new("session_closing",Some("bad"),None).to_string(),"session_closing");}
}
