use std::collections::BTreeMap;
use std::time::Duration;
use maho_pty::PtySessionOptions;
use crate::runtime_session::{TerminalRuntimeSession,RuntimeError};
use crate::shared::{DEFAULT_MAX_SESSIONS,KILLED_SESSION_EXIT_GRACE_MS};
#[derive(Debug,thiserror::Error)]
pub enum ManagerError {#[error("Cannot create terminal session: capacity limit ({0}) reached.")] Capacity(usize),#[error(transparent)] Runtime(#[from] RuntimeError)}
struct Entry {runtime:TerminalRuntimeSession,created_at:u64,last_used_at:u64}
pub struct TerminalManager {entries:BTreeMap<String,Entry>,monitor_ids:BTreeMap<String,String>,max_sessions:usize,next_id:u64,sequence:u64,reservations:std::sync::Arc<std::sync::atomic::AtomicUsize>}
pub struct CapacityReservation(std::sync::Arc<std::sync::atomic::AtomicUsize>);
impl Drop for CapacityReservation {fn drop(&mut self) {self.0.fetch_sub(1,std::sync::atomic::Ordering::SeqCst);}}
impl Default for TerminalManager {fn default()->Self {Self::new(DEFAULT_MAX_SESSIONS)}}
impl TerminalManager {
    pub fn new(max_sessions:usize)->Self {Self {entries:BTreeMap::new(),monitor_ids:BTreeMap::new(),max_sessions,next_id:0,sequence:0,reservations:Default::default()}}
    pub fn size(&self)->usize {self.entries.len()}
    pub fn active_size(&self)->Result<usize,RuntimeError> {self.entries.values().try_fold(self.reservations.load(std::sync::atomic::Ordering::SeqCst),|size,entry|Ok(size+usize::from(!entry.runtime.exited()?)))}
    pub fn reserve(&mut self)->Result<Option<CapacityReservation>,RuntimeError> {if self.active_size()?>=self.max_sessions {return Ok(None);}self.reservations.fetch_add(1,std::sync::atomic::Ordering::SeqCst);Ok(Some(CapacityReservation(self.reservations.clone())))}
    pub fn create(&mut self,command:&str,options:PtySessionOptions)->Result<String,ManagerError> {
        if self.active_size()?>=self.max_sessions {return Err(ManagerError::Capacity(self.max_sessions));}
        while self.entries.len()>=self.max_sessions {
            let mut selected=None;for (id,entry) in &self.entries {if entry.runtime.exited()? {let key=(entry.last_used_at,entry.created_at);if selected.as_ref().is_none_or(|(_,best)|key<*best) {selected=Some((id.clone(),key));}}}
            let Some((id,_))=selected else {return Err(ManagerError::Capacity(self.max_sessions));};
            if let Some(entry)=self.entries.remove(&id) {entry.runtime.dispose()?;}
            self.monitor_ids.retain(|_,runtime_id|runtime_id!=&id);
        }
        let runtime=TerminalRuntimeSession::start(command,options)?;self.next_id+=1;self.sequence+=1;let id=format!("bash_{}",self.next_id);self.entries.insert(id.clone(),Entry {runtime,created_at:self.sequence,last_used_at:self.sequence});Ok(id)
    }
    pub fn get(&mut self,id:&str)->Option<&mut TerminalRuntimeSession> {self.sequence+=1;let entry=self.entries.get_mut(id)?;entry.last_used_at=self.sequence;Some(&mut entry.runtime)}
    pub fn bind_monitor_id(&mut self,monitor_id:&str,session_id:&str) {self.monitor_ids.insert(monitor_id.to_owned(),session_id.to_owned());}
    pub fn resolve_id(&self,id:&str)->Option<String> {if id.starts_with("mon_") {self.monitor_ids.get(id).cloned()} else if self.entries.contains_key(id) {Some(id.to_owned())} else {None}}
    pub fn list(&self)->Vec<(&str,&TerminalRuntimeSession)> {let mut entries=self.entries.iter().collect::<Vec<_>>();entries.sort_by_key(|(_,entry)|entry.created_at);entries.into_iter().map(|(id,entry)|(id.as_str(),&entry.runtime)).collect()}
    pub fn stop(&mut self,id:&str)->Result<bool,RuntimeError> {let Some(entry)=self.entries.get_mut(id) else {return Ok(false);};if !entry.runtime.exited()? {entry.runtime.kill()?;entry.runtime.wait(Duration::from_millis(KILLED_SESSION_EXIT_GRACE_MS))?;}Ok(true)}
    pub fn teardown(&mut self)->Result<(),RuntimeError> {for id in self.entries.keys().cloned().collect::<Vec<_>>() {self.stop(&id)?;}for (_,entry) in std::mem::take(&mut self.entries) {entry.runtime.dispose()?;}self.monitor_ids.clear();self.next_id=0;Ok(())}
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn reservation_counts_until_release()->Result<(),RuntimeError> {let mut manager=TerminalManager::new(1);let reservation=manager.reserve()?.expect("capacity");assert_eq!(manager.active_size()?,1);assert!(manager.reserve()?.is_none());drop(reservation);assert_eq!(manager.active_size()?,0);assert!(manager.reserve()?.is_some());Ok(())}
    #[test] fn capacity_stop_and_monitor_bindings()->Result<(),ManagerError> {let mut manager=TerminalManager::new(1);let id=manager.create("read",PtySessionOptions::new("/bin/sh").arg("-c").arg("read value"))?;manager.bind_monitor_id("mon_1",&id);assert_eq!(manager.resolve_id("mon_1"),Some(id.clone()));assert_eq!(manager.active_size()?,1);assert!(matches!(manager.create("read",PtySessionOptions::new("/bin/sh").arg("-c").arg("read value")),Err(ManagerError::Capacity(1))));assert!(manager.stop(&id)?);assert!(manager.get(&id).is_some());manager.teardown()?;assert_eq!(manager.size(),0);assert_eq!(manager.resolve_id("mon_1"),None);Ok(())}
}
