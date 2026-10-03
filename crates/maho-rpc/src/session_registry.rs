use std::{collections::BTreeMap,path::{Path,PathBuf},sync::Arc};
pub struct RpcSessionLaunchProfile{pub runtime:maho_core::agent_session_runtime::AgentSessionLaunchProfile,pub session_path:Option<String>,pub durable_session_id:Option<String>,pub session_kind:Option<maho_ext_api::SessionKind>,pub session_context:Option<BTreeMap<String,String>>}
pub struct SessionIdentity{pub kind:maho_ext_api::SessionKind,pub context:Arc<BTreeMap<String,String>>}
#[derive(Debug,Clone,Copy,PartialEq,Eq)]pub enum RpcSessionState{Opening,Open,Closing,Quarantined,Closed}
pub struct SessionCloseState{pub state:RpcSessionState,pub attachments:i64,pub retain_on_disconnect:bool,pub reservation_key:Option<String>,pub writing:bool}
pub struct DurableSessionIdentity{pub state:RpcSessionState,pub durable_session_id:Option<String>,pub reservation_key:Option<String>}
pub struct RpcSessionEntry{
    pub close:SessionCloseState,
    pub profile:RpcSessionLaunchProfile,
    pub identity:SessionIdentity,
    pub scope:maho_ai::node::provider_scope::ProviderScope,
    pub runtime:Option<Arc<maho_core::agent_session_runtime::AgentSessionRuntime>>,
    pub durable_session_id:Option<String>,
    pub session_path:Option<String>,
    pub last_command_at:f64,
}
#[derive(Default)]pub struct RpcSessionRegistry{entries:BTreeMap<String,RpcSessionEntry>,next_handle:u64,worker_refusal:Option<(u64,u64)>}
pub enum OpenAdmission{Create(String),Attached(String)}
impl RpcSessionRegistry{
    pub fn size(&self)->usize{self.entries.len()}
    pub fn get(&self,handle:&str)->Option<&RpcSessionEntry>{self.entries.get(handle)}
    pub fn get_mut(&mut self,handle:&str)->Option<&mut RpcSessionEntry>{self.entries.get_mut(handle)}
    pub fn get_for_command(&mut self,handle:&str,command:&str,now:f64)->Result<&mut RpcSessionEntry,RpcSessionRegistryError>{
        let entry=self.entries.get_mut(handle).ok_or_else(||RpcSessionRegistryError::new("unknown_session",None,None))?;
        if entry.close.state==RpcSessionState::Closing&&!matches!(command,"abort"|"abort_bash"|"extension_ui_response"|"extension_ui_progress"){return Err(RpcSessionRegistryError::new("session_closing",None,None));}
        if !matches!(entry.close.state,RpcSessionState::Open|RpcSessionState::Closing){return Err(RpcSessionRegistryError::new("unknown_session",None,None));}
        entry.last_command_at=now;Ok(entry)
    }
    pub fn set_worker_admission(&mut self,refusal:Option<(u64,u64)>){self.worker_refusal=refusal;}
    pub async fn dispatch_command(&mut self,handle:&str,command:&crate::rpc_types::RpcCommand,kind:&str,now:f64,activity:&crate::session_attribution::SessionActivityRegistry)->Result<Option<crate::rpc_types::RpcResponse>,RpcSessionRegistryError>{
        let entry=self.get_for_command(handle,kind,now)?;
        let runtime=entry.runtime.as_ref().ok_or_else(||RpcSessionRegistryError::new("unknown_session",None,None))?;
        crate::session_attribution::run_with_session_attribution(activity,crate::session_attribution::SessionAttribution{session_id:Some(handle.into()),tool:None},async{
            maho_ai::node::provider_scope::run_with_provider_scope_async(&entry.scope,crate::connection_handler::handle_session_command(runtime.session(),command)).await.map_err(|error|RpcSessionRegistryError::new("session_closed",Some(&error.to_string()),None)).map(|response|response.map(|mut response|{response.session_id=Some(handle.into());response}))
        }).await
    }
    pub fn admit_open(&mut self,mut profile:RpcSessionLaunchProfile,retain:bool,now:f64)->Result<OpenAdmission,RpcSessionRegistryError>{
        validate_profile_paths(&profile)?;
        if let Some(path)=&profile.session_path{profile.session_path=Some(canonical_path(Path::new(path)).map_err(|error|RpcSessionRegistryError::new("invalid_path",Some(&error.to_string()),None))?.to_string_lossy().into_owned());}
        let entries=self.entries.values().map(|entry|DurableSessionIdentity{state:entry.close.state,durable_session_id:entry.durable_session_id.clone(),reservation_key:entry.close.reservation_key.clone()}).collect::<Vec<_>>();
        check_durable_session_collision(profile.durable_session_id.as_deref(),profile.session_path.as_deref(),&entries)?;
        if let Some(path)=&profile.session_path&&let Some((handle,entry))=self.entries.iter_mut().find(|(_,entry)|entry.close.reservation_key.as_ref()==Some(path)){
            if entry.close.state!=RpcSessionState::Open||entry.durable_session_id.is_none(){return Err(RpcSessionRegistryError::new("session_path_in_use",None,None));}
            entry.close.attachments+=1;entry.close.retain_on_disconnect|=retain;entry.last_command_at=now;return Ok(OpenAdmission::Attached(handle.clone()));
        }
        if profile.session_kind==Some(maho_ext_api::SessionKind::Worker)&&let Some((rss,retry))=self.worker_refusal{return Err(RpcSessionRegistryError::new("host_memory_pressure",None,Some(serde_json::json!({"rssMb":rss,"retry_after_ms":retry}))));}
        self.next_handle+=1;let handle=format!("rpc-{}",self.next_handle);let identity=session_identity(&profile);
        self.entries.insert(handle.clone(),RpcSessionEntry{close:SessionCloseState{state:RpcSessionState::Opening,attachments:1,retain_on_disconnect:retain,reservation_key:profile.session_path.clone(),writing:false},scope:maho_ai::node::provider_scope::ProviderScope::new(),runtime:None,durable_session_id:profile.durable_session_id.clone(),session_path:profile.session_path.clone(),profile,identity,last_command_at:now});
        Ok(OpenAdmission::Create(handle))
    }
    pub fn install_runtime(&mut self,handle:&str,runtime:Arc<maho_core::agent_session_runtime::AgentSessionRuntime>)->Result<(),RpcSessionRegistryError>{
        let entry=self.entries.get_mut(handle).filter(|entry|entry.close.state==RpcSessionState::Opening).ok_or_else(||RpcSessionRegistryError::new("unknown_session",None,None))?;
        entry.durable_session_id=Some(runtime.session().session_id());entry.session_path=runtime.session().session_file();entry.close.writing=true;entry.close.state=RpcSessionState::Open;entry.runtime=Some(runtime);Ok(())
    }
    pub async fn create_admitted_runtime(&mut self,handle:&str,create:impl std::future::Future<Output=Result<maho_core::agent_session_runtime::AgentSessionRuntime,String>>)->Result<(),RpcSessionRegistryError>{
        let scope=self.entries.get(handle).filter(|entry|entry.close.state==RpcSessionState::Opening).ok_or_else(||RpcSessionRegistryError::new("unknown_session",None,None))?.scope.clone();
        let created=maho_ai::node::provider_scope::run_with_provider_scope_async(&scope,create).await;
        match created{
            Ok(Ok(runtime))=>self.install_runtime(handle,Arc::new(runtime)),
            result=>{
                self.rollback_open(handle).await;
                let reason=match result{Ok(Err(reason))=>reason,Err(reason)=>reason.to_string(),Ok(Ok(_))=>unreachable!()};
                Err(RpcSessionRegistryError::new("open_failed",Some(&reason),None))
            }
        }
    }
    pub async fn rollback_open(&mut self,handle:&str){
        if let Some(entry)=self.entries.get(handle){if let Some(runtime)=&entry.runtime{runtime.dispose().await;}entry.scope.close();}
        self.entries.remove(handle);
    }
    pub fn list_sessions(&self,include_workers:bool)->Vec<serde_json::Value>{
        self.entries.iter().filter(|(_,entry)|entry.close.state!=RpcSessionState::Quarantined&&(include_workers||entry.identity.kind!=maho_ext_api::SessionKind::Worker)).map(|(handle,entry)|{
            let status=match entry.close.state{RpcSessionState::Opening=>"opening",RpcSessionState::Open=>"open",RpcSessionState::Closing=>"closing",RpcSessionState::Quarantined=>"quarantined",RpcSessionState::Closed=>"closed"};
            let mut row=serde_json::json!({"sessionId":handle,"cwd":entry.runtime.as_ref().map_or(entry.profile.runtime.cwd.as_str(),|runtime|runtime.cwd()),"status":status,"attachments":entry.close.attachments,"kind":if entry.identity.kind==maho_ext_api::SessionKind::Worker{"worker"}else{"interactive"}});
            if let Some(id)=&entry.durable_session_id{row["durableSessionId"]=id.clone().into();}
            if let Some(path)=&entry.session_path{row["sessionPath"]=path.clone().into();}
            if let Some(name)=entry.runtime.as_ref().and_then(|runtime|runtime.session().session_name()){row["name"]=name.into();}
            if include_workers{row["context"]=serde_json::json!(&*entry.identity.context);}
            row
        }).collect()
    }
    pub fn mark_close(&mut self,handle:&str,detach:bool)->Result<crate::session_teardown::CloseClaim,RpcSessionRegistryError>{
        let entry=self.entries.get_mut(handle).ok_or_else(||RpcSessionRegistryError::new("unknown_session",None,None))?;
        crate::session_teardown::begin_session_close(&mut entry.close,detach)
    }
    pub async fn close_marked(&mut self,handle:&str,grace_ms:u64)->Result<Option<crate::session_teardown::RuntimeCloseCompletion>,RpcSessionRegistryError>{
        let entry=self.entries.get(handle).filter(|entry|entry.close.state==RpcSessionState::Closing).ok_or_else(||RpcSessionRegistryError::new("unknown_session",None,None))?;
        let completion=if let Some(runtime)=&entry.runtime{Some(crate::session_teardown::close_runtime_with_grace(Arc::clone(runtime),entry.scope.clone(),grace_ms).await)}else{entry.scope.close();None};
        self.entries.remove(handle);Ok(completion)
    }
}
pub fn validate_profile_paths(profile:&RpcSessionLaunchProfile)->Result<(),RpcSessionRegistryError>{if !Path::new(&profile.runtime.cwd).is_absolute()||profile.session_path.as_ref().is_some_and(|path|!Path::new(path).is_absolute()){Err(RpcSessionRegistryError::new("invalid_path",None,None))}else{Ok(())}}
pub fn check_durable_session_collision(requested_id:Option<&str>,session_path:Option<&str>,entries:&[DurableSessionIdentity])->Result<(),RpcSessionRegistryError>{
    if let Some(requested_id)=requested_id{for entry in entries{
        if entry.state==RpcSessionState::Closed||entry.durable_session_id.as_deref()!=Some(requested_id){continue;}
        if session_path.is_some()&&entry.reservation_key.as_deref()==session_path{continue;}
        return Err(RpcSessionRegistryError::new("session_id_in_use",None,None));
    }}
    Ok(())
}
pub fn session_identity(profile:&RpcSessionLaunchProfile)->SessionIdentity{SessionIdentity{kind:profile.session_kind.unwrap_or_default(),context:Arc::new(profile.session_context.clone().unwrap_or_default())}}
pub fn canonical_path(path:&Path)->std::io::Result<PathBuf>{if path.exists(){path.canonicalize()}else{let parent=path.parent().unwrap_or(Path::new("."));Ok(parent.canonicalize()?.join(path.file_name().unwrap_or_default()))}}
#[derive(Debug,thiserror::Error,PartialEq,Eq)]#[error("{code}{suffix}")]
pub struct RpcSessionRegistryError{pub code:String,pub detail:Option<serde_json::Value>,suffix:String}
impl RpcSessionRegistryError{pub fn new(code:&str,reason:Option<&str>,detail:Option<serde_json::Value>)->Self{Self{code:code.into(),detail,suffix:if code=="open_failed"{reason.filter(|reason|!reason.is_empty()).map_or(String::new(),|reason|format!(": {reason}"))}else{String::new()}}}}
#[cfg(test)]mod tests{
    use super::*;
    #[test]fn launch_paths_must_be_absolute_before_creation(){let mut profile=RpcSessionLaunchProfile{runtime:Default::default(),session_path:None,durable_session_id:None,session_kind:None,session_context:None};profile.runtime.cwd="relative".into();assert_eq!(validate_profile_paths(&profile).unwrap_err().code,"invalid_path");profile.runtime.cwd="/absolute".into();assert!(validate_profile_paths(&profile).is_ok());profile.session_path=Some("relative".into());assert!(validate_profile_paths(&profile).is_err());profile.session_path=Some("/session".into());assert!(validate_profile_paths(&profile).is_ok());}
    #[test]fn durable_collision_is_live_only_and_same_reserved_path_may_attach(){let entries=[DurableSessionIdentity{state:RpcSessionState::Opening,durable_session_id:Some("durable".into()),reservation_key:Some("/same".into())}];assert!(check_durable_session_collision(Some("durable"),Some("/same"),&entries).is_ok());assert_eq!(check_durable_session_collision(Some("durable"),Some("/other"),&entries).unwrap_err().code,"session_id_in_use");assert!(check_durable_session_collision(None,None,&entries).is_ok());let entries=[DurableSessionIdentity{state:RpcSessionState::Closed,durable_session_id:Some("durable".into()),reservation_key:None}];assert!(check_durable_session_collision(Some("durable"),None,&entries).is_ok());}
    #[test]fn identity_defaults_and_context_do_not_alias_client_inputs(){let mut profile=RpcSessionLaunchProfile{runtime:Default::default(),session_path:None,durable_session_id:None,session_kind:None,session_context:Some(BTreeMap::from([("owner".into(),"a".into())]))};let identity=session_identity(&profile);profile.session_context.as_mut().unwrap().insert("owner".into(),"b".into());assert_eq!(identity.kind,maho_ext_api::SessionKind::Interactive);assert_eq!(identity.context["owner"],"a");}
    #[test]fn canonical_missing_leaf_uses_real_parent(){let temp=tempfile::tempdir().unwrap();let parent=temp.path().join("real");std::fs::create_dir(&parent).unwrap();#[cfg(unix)]{let alias=temp.path().join("alias");std::os::unix::fs::symlink(&parent,&alias).unwrap();assert_eq!(canonical_path(&alias.join("missing.jsonl")).unwrap(),parent.join("missing.jsonl"));}}
    #[test]fn only_open_failed_appends_reason(){assert_eq!(RpcSessionRegistryError::new("open_failed",Some("bad"),None).to_string(),"open_failed: bad");assert_eq!(RpcSessionRegistryError::new("session_closing",Some("bad"),None).to_string(),"session_closing");}
}
