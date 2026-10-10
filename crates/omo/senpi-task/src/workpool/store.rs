use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use super::types::{WorkpoolCaller, WorkpoolCreate, WorkpoolError, WorkpoolErrorCode, WorkpoolMode, WorkpoolRecord, WorkpoolSpec};

#[derive(Clone, Default)] pub struct WorkpoolStore { state_dir: std::path::PathBuf, records: Arc<Mutex<BTreeMap<String, WorkpoolRecord>>> }
impl WorkpoolStore {
 pub fn new(state_dir: impl Into<std::path::PathBuf>) -> Self { Self{state_dir:state_dir.into(),records:Default::default()} }
 pub fn state_dir(&self)->&std::path::Path { &self.state_dir }
 pub fn load(&self,pool_id:&str)->Result<WorkpoolRecord,WorkpoolError>{ self.records.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(pool_id).cloned().ok_or_else(||WorkpoolError::new(WorkpoolErrorCode::PoolNotFound,"Pool not found.")) }
 pub fn list(&self)->Vec<WorkpoolRecord>{self.records.lock().unwrap_or_else(std::sync::PoisonError::into_inner).values().cloned().collect()}
 pub fn owned(&self,caller:&WorkpoolCaller,pool_id:&str)->Result<WorkpoolRecord,WorkpoolError>{let r=self.load(pool_id)?;if r.parent_session_id!=caller.session_id{ return Err(WorkpoolError::new(WorkpoolErrorCode::ScopeDenied,"Only the owning parent session may access this pool."));}Ok(r)}
 pub fn mutate(&self,pool_id:&str,update:impl FnOnce(&WorkpoolRecord)->WorkpoolRecord)->Result<WorkpoolRecord,WorkpoolError>{let mut all=self.records.lock().unwrap_or_else(std::sync::PoisonError::into_inner);let prior=all.get(pool_id).cloned().ok_or_else(||WorkpoolError::new(WorkpoolErrorCode::PoolNotFound,"Pool not found."))?;let next=update(&prior);all.insert(pool_id.to_string(),next.clone());Ok(next)}
 pub fn create(&self,caller:&WorkpoolCaller,input:&WorkpoolCreate,mode:WorkpoolMode,worker_spec:WorkpoolSpec)->Result<WorkpoolRecord,WorkpoolError>{let mut all=self.records.lock().unwrap_or_else(std::sync::PoisonError::into_inner);if let Some(existing)=all.values().find(|p|p.parent_session_id==caller.session_id&&p.name==input.name){let mut requested=input.tools.clone().unwrap_or_default();let mut granted=existing.kernel_tool_names.clone().unwrap_or_default();requested.sort();granted.sort();if existing.mode==mode&&existing.status==super::types::WorkpoolStatus::Open&&granted==requested{return Ok(existing.clone())}return Err(WorkpoolError::new(WorkpoolErrorCode::PoolNameConflict,"Pool name is already in use; choose a new explicit name."));}let id=format!("wp_{:x}",u128::from_le_bytes(rand_bytes()));let r=WorkpoolRecord{version:1,pool_id:id.clone(),name:input.name.clone(),parent_session_id:caller.session_id.clone(),root_session_id:caller.root_session_id.clone(),generation:1,revision:0,mode,agent:input.agent.clone(),worker_spec,status:super::types::WorkpoolStatus::Open,kernel_tool_names:input.tools.clone(),items:vec![],workers:vec![],aggregate:None};all.insert(id,r.clone());Ok(r)}
}
fn rand_bytes()->[u8;16]{use std::time::{SystemTime,UNIX_EPOCH};let n=SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();n.to_le_bytes()}
pub type WorkpoolStoreHandle=WorkpoolStore;
#[cfg(test)] mod tests{use super::*;#[test]fn missing_pool_is_typed(){let s=WorkpoolStore::new("/tmp");assert_eq!(s.load("wp_missing").err().expect("missing").code,WorkpoolErrorCode::PoolNotFound);}}
