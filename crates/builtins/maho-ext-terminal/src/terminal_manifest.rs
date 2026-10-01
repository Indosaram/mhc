use std::collections::BTreeMap;
use maho_core::session_sidecar_store::{SidecarStore,SidecarError,CreateSidecarStoreOptions,create_sidecar_store};
use crate::terminal_manifest_model::*;

pub fn create_terminal_manifest_store(session_dir:&std::path::Path,session_id:&str)->SidecarStore {
    create_sidecar_store(CreateSidecarStoreOptions {base_dir:session_dir.join("extensions/terminal").to_string_lossy().into_owned(),session_id:session_id.to_owned(),version:i64::from(TERMINAL_MANIFEST_VERSION),temp_prefix:"terminal".to_owned(),parse:std::sync::Arc::new(|value,reference| {
        let manifest=crate::restore::parse_terminal_manifest(value,&reference.session_id).map_err(|error|SidecarError::Invalid(error.to_string()))?;
        serde_json::to_value(manifest).map_err(|error|SidecarError::Invalid(error.to_string()))
    })})
}
pub struct TerminalManifestWriter {
    pub store:SidecarStore,
    session_id:String,
    entries:BTreeMap<String,ManifestMonitor>,
    backgrounds:BTreeMap<String,ManifestBackgroundSession>,
    pending:BTreeMap<String,TerminalManifestCheckpoint>,
    pub persist_failure:Option<SidecarError>,
}
impl TerminalManifestWriter {
    pub fn new(session_dir:&std::path::Path,session_id:&str)->Self {Self {store:create_terminal_manifest_store(session_dir,session_id),session_id:session_id.to_owned(),entries:BTreeMap::new(),backgrounds:BTreeMap::new(),pending:BTreeMap::new(),persist_failure:None}}
    pub async fn record_register(&mut self,registration:MonitorRegistration,now:f64) {
        let (description,runtime_kind,command,path,event,filter,cwd,approved_parent,persistent)=match registration.spec {
            MonitorSpec::Command {description,command,filter,cwd,persistent}=>(description,MonitorRuntimeKind::Command,Some(command),None,None,filter,cwd,None,persistent),
            MonitorSpec::File {description,path,event,cwd,approved_parent,persistent,..}=>(description,MonitorRuntimeKind::File,None,Some(path),Some(event),None,Some(cwd),approved_parent,persistent),
        };
        let durability_class=if !persistent {MonitorDurabilityClass::Ephemeral} else if runtime_kind==MonitorRuntimeKind::File {MonitorDurabilityClass::CheckpointedFile} else {MonitorDurabilityClass::RestartableCommand};
        self.entries.insert(registration.monitor_id.clone(),ManifestMonitor {monitor_id:registration.monitor_id,session_id:self.session_id.clone(),description,runtime_kind,durability_class,command,path,event,filter,cwd,approved_parent,created_at:now,expires_at:persistent.then_some(now+DURABLE_MONITOR_EXPIRY_MS as f64),persistent,suspended:false,last_checkpoint:None,delivery_paused:false,fire_window:ManifestFireWindow {start_ms:now,count:0.0}});
        self.persist(now).await;
    }
    pub fn adopt_restored(&mut self,mut entry:ManifestMonitor) {entry.suspended=false;self.entries.insert(entry.monitor_id.clone(),entry);}
    pub fn durable_count(&self)->usize {self.entries.values().filter(|entry|entry.durability_class!=MonitorDurabilityClass::Ephemeral).count()}
    pub async fn observe_monitor_state(&mut self,snapshot:&[crate::monitor_registry::MonitorSnapshotEntry],now:f64)->Result<(),String> {
        let mut live=BTreeMap::new();
        for entry in snapshot {
            let id=entry.monitor_id.as_deref().filter(|id|!id.is_empty()).ok_or_else(||format!("terminal manifest writer saw a monitor snapshot entry without a stable monitorId (runtime id {}); surfacing instead of silently skipping it",entry.id))?;
            live.insert(id,entry);
        }
        let mut changed=false;
        self.entries.retain(|id,entry| {
            let Some(current)=live.get(id.as_str()) else {if entry.suspended {return true;}changed=true;return false;};
            let window=current.fire_window.as_ref().map(|window|ManifestFireWindow {start_ms:window.start_ms,count:window.count as f64}).unwrap_or_else(||entry.fire_window.clone());
            if entry.delivery_paused!=current.paused||entry.fire_window.count!=window.count {entry.delivery_paused=current.paused;entry.fire_window=window;changed=true;}
            true
        });
        if changed {self.persist(now).await;}Ok(())
    }
    pub fn schedule_checkpoint(&mut self,id:&str,checkpoint:TerminalManifestCheckpoint) {self.pending.insert(id.to_owned(),checkpoint);}
    fn absorb_pending(&mut self) {for (id,checkpoint) in std::mem::take(&mut self.pending) {if let Some(entry)=self.entries.get_mut(&id) {entry.last_checkpoint=Some(checkpoint);}}}
    pub async fn flush(&mut self,now:f64) {if !self.pending.is_empty() {self.absorb_pending();self.persist(now).await;}}
    pub async fn record_shutdown(&mut self,now:f64) {self.absorb_pending();for entry in self.entries.values_mut() {entry.suspended=true;}self.persist(now).await;}
    pub async fn record_background_start(&mut self,id:&str,command:&str,started_at_ms:f64,now:f64) {self.backgrounds.insert(id.to_owned(),ManifestBackgroundSession {id:id.to_owned(),command:command.to_owned(),started_at_ms});self.persist(now).await;}
    pub async fn record_background_exit(&mut self,id:&str,now:f64) {if self.backgrounds.remove(id).is_some() {self.persist(now).await;}}
    async fn persist(&mut self,now:f64) {
        let state=TerminalManifest {version:TERMINAL_MANIFEST_VERSION,session_id:self.session_id.clone(),monitors:self.entries.values().cloned().collect(),background_sessions:self.backgrounds.values().cloned().collect(),updated_at:now};
        let result=match serde_json::to_value(state) {Ok(value)=>self.store.write(&value).await,Err(error)=>Err(SidecarError::Invalid(error.to_string()))};
        if let Err(error)=result {self.persist_failure=Some(error);}
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn registration_checkpoint_shutdown_and_adoption_preserve_deadline() {
        let dir=tempfile::tempdir().unwrap();let mut writer=TerminalManifestWriter::new(dir.path(),"s");
        writer.record_register(MonitorRegistration {monitor_id:"mon_1".to_owned(),spec:MonitorSpec::Command {description:"watch".to_owned(),command:"true".to_owned(),filter:None,cwd:Some("/tmp".to_owned()),persistent:true}},10.0).await;
        assert_eq!(writer.durable_count(),1);assert!(writer.persist_failure.is_none());
        let checkpoint=TerminalManifestCheckpoint {dev:1.0,ino:2.0,size:3.0,mtime_ms:4.0,digest:"saved".to_owned(),present:true};writer.schedule_checkpoint("mon_1",checkpoint.clone());writer.record_shutdown(20.0).await;
        let state=crate::restore::parse_terminal_manifest(&writer.store.read().await.unwrap().unwrap(),"s").unwrap();let entry=state.monitors[0].clone();assert!(entry.suspended);assert_eq!(entry.last_checkpoint,Some(checkpoint));
        let mut next=TerminalManifestWriter::new(dir.path(),"s");next.adopt_restored(entry.clone());assert!(!next.entries["mon_1"].suspended);next.record_background_start("bash_2","read",30.0,30.0).await;
        let state=crate::restore::parse_terminal_manifest(&next.store.read().await.unwrap().unwrap(),"s").unwrap();assert_eq!(state.monitors[0].expires_at,entry.expires_at);assert_eq!(state.monitors[0].created_at,10.0);
        next.observe_monitor_state(&[],40.0).await.unwrap();assert_eq!(next.durable_count(),0);
    }
    #[tokio::test]
    async fn missing_stable_identity_is_an_error() {let dir=tempfile::tempdir().unwrap();let mut writer=TerminalManifestWriter::new(dir.path(),"s");assert!(writer.observe_monitor_state(&[crate::monitor_registry::MonitorSnapshotEntry {id:"bash_1".to_owned(),..Default::default()}],0.0).await.is_err());}
}
