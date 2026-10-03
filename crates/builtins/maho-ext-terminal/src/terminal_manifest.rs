use std::collections::BTreeMap;
use indexmap::IndexMap;
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
    entries:IndexMap<String,ManifestMonitor>,
    backgrounds:IndexMap<String,ManifestBackgroundSession>,
    pending:BTreeMap<String,TerminalManifestCheckpoint>,
    pub persist_failure:Option<SidecarError>,
}
impl TerminalManifestWriter {
    pub fn new(session_dir:&std::path::Path,session_id:&str)->Self {Self {store:create_terminal_manifest_store(session_dir,session_id),session_id:session_id.to_owned(),entries:IndexMap::new(),backgrounds:IndexMap::new(),pending:BTreeMap::new(),persist_failure:None}}
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
    #[cfg(unix)]
    pub async fn restore_live(&mut self,manager:&mut crate::manager::TerminalManager,registry:&mut crate::monitor_registry::MonitorRegistry,now:f64)->crate::restore::RestoreDigest {
        self.restore_configured_live(manager,registry,now,None,&crate::settings::TERMINAL_SETTINGS_DEFAULTS).await
    }
    #[cfg(unix)]
    pub async fn restore_configured_live(&mut self,manager:&mut crate::manager::TerminalManager,registry:&mut crate::monitor_registry::MonitorRegistry,now:f64,shell:Option<&str>,settings:&crate::settings::ResolvedTerminalSettings)->crate::restore::RestoreDigest {
        use crate::restore::{RestoreDigest,RestoreOutcome};
        let mut digest=RestoreDigest::default();
        let state=match self.store.read().await {Ok(None)=>return digest,Ok(Some(value))=>match crate::restore::parse_terminal_manifest(&value,&self.session_id) {Ok(state)=>state,Err(_)=>{digest.store_error=true;return digest;}},Err(_)=>{digest.store_error=true;return digest;}};
        for monitor in state.monitors {
            if monitor.expires_at.is_some_and(|expiry|expiry<=now) {digest.expired+=1;continue;}
            let outcome=match monitor.durability_class {
                MonitorDurabilityClass::Ephemeral=>RestoreOutcome::Lost,
                MonitorDurabilityClass::RestartableCommand=>crate::durable_command::restore_configured_command(&monitor,manager,shell,settings,|_,runtime,record|registry.register(runtime,record)),
                MonitorDurabilityClass::CheckpointedFile=>crate::durable_file::restore_file(&monitor,registry,manager,None,now),
            };
            match outcome {RestoreOutcome::Restored=>digest.restored+=1,RestoreOutcome::Muted=>digest.muted+=1,RestoreOutcome::Lost=>digest.lost+=1,RestoreOutcome::AttachedElsewhere=>digest.attached_elsewhere+=1}
            if matches!(outcome,RestoreOutcome::Restored|RestoreOutcome::Muted) {
                self.adopt_restored(monitor.clone());registry.adopt_fire_window(&monitor.monitor_id,&monitor.fire_window);
                if let Some(id)=manager.resolve_id(&monitor.monitor_id)&&let Some(checkpoint)=registry.file_checkpoint(&id) {self.schedule_checkpoint(&monitor.monitor_id,checkpoint);}
            }
        }
        digest.lost+=state.background_sessions.len();digest
    }
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
    pub async fn record_background_exit(&mut self,id:&str,now:f64) {if self.backgrounds.shift_remove(id).is_some() {self.persist(now).await;}}
    async fn persist(&mut self,now:f64) {
        let state=TerminalManifest {version:TERMINAL_MANIFEST_VERSION,session_id:self.session_id.clone(),monitors:self.entries.values().cloned().collect(),background_sessions:self.backgrounds.values().cloned().collect(),updated_at:now};
        let result=match serde_json::to_value(state) {Ok(value)=>self.store.write(&value).await,Err(error)=>Err(SidecarError::Invalid(error.to_string()))};
        if let Err(error)=result {self.persist_failure=Some(error);}
    }
}
pub struct CheckpointDebouncer {
    writer:std::sync::Arc<tokio::sync::Mutex<TerminalManifestWriter>>,
    task:Option<tokio::task::JoinHandle<()>>,
}
impl CheckpointDebouncer {
    pub fn new(writer:std::sync::Arc<tokio::sync::Mutex<TerminalManifestWriter>>)->Self {Self {writer,task:None}}
    pub async fn schedule(&mut self,id:&str,checkpoint:TerminalManifestCheckpoint) {
        self.writer.lock().await.schedule_checkpoint(id,checkpoint);
        if let Some(task)=self.task.take() {task.abort();}
        let writer=self.writer.clone();
        let deadline=tokio::time::Instant::now()+std::time::Duration::from_millis(TERMINAL_MANIFEST_CHECKPOINT_DEBOUNCE_MS);
        self.task=Some(tokio::spawn(async move {
            tokio::time::sleep_until(deadline).await;
            let now=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("epoch").as_secs_f64()*1000.0;
            writer.lock().await.flush(now).await;
        }));
    }
    pub async fn flush(&mut self,now:f64) {if let Some(task)=self.task.take() {task.abort();}self.writer.lock().await.flush(now).await;}
    pub async fn record_shutdown(&mut self,now:f64) {if let Some(task)=self.task.take() {task.abort();}self.writer.lock().await.record_shutdown(now).await;}
}
impl Drop for CheckpointDebouncer {fn drop(&mut self) {if let Some(task)=self.task.take() {task.abort();}}}
#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    #[tokio::test]
    async fn restore_store_adopts_live_file_and_preserves_mute_deadline_and_checkpoint() {
        let dir=tempfile::tempdir().unwrap();let path=dir.path().join("watch");std::fs::write(&path,b"same").unwrap();let checkpoint=crate::durable_file::file_checkpoint(&path).unwrap();let mut writer=TerminalManifestWriter::new(dir.path(),"s");
        writer.record_register(MonitorRegistration {monitor_id:"mon_saved".to_owned(),spec:MonitorSpec::File {description:"watch".to_owned(),path:"watch".to_owned(),event:FileEvent::Modify,timeout_ms:300_000.0,cwd:dir.path().to_string_lossy().into_owned(),approved_parent:None,persistent:true}},10.0).await;writer.entries.get_mut("mon_saved").unwrap().delivery_paused=true;writer.schedule_checkpoint("mon_saved",checkpoint.clone());writer.record_shutdown(20.0).await;
        let expiry=writer.entries["mon_saved"].expires_at;let mut next=TerminalManifestWriter::new(dir.path(),"s");let mut manager=crate::manager::TerminalManager::default();let mut registry=crate::monitor_registry::MonitorRegistry::new(|_|{});let digest=next.restore_live(&mut manager,&mut registry,30.0).await;
        assert_eq!(digest,crate::restore::RestoreDigest {muted:1,..Default::default()});assert_eq!(next.durable_count(),1);assert!(!next.entries["mon_saved"].suspended);assert_eq!(next.entries["mon_saved"].expires_at,expiry);assert!(registry.snapshot()[0].paused);assert_eq!(manager.resolve_id("mon_saved"),Some("watch_1".to_owned()));next.flush(40.0).await;
        let persisted=crate::restore::parse_terminal_manifest(&next.store.read().await.unwrap().unwrap(),"s").unwrap();assert_eq!(persisted.monitors[0].last_checkpoint,Some(checkpoint));registry.dispose();manager.teardown().unwrap();
    }
    #[tokio::test]
    async fn background_records_follow_insertion_order() {
        let dir=tempfile::tempdir().unwrap();let mut writer=TerminalManifestWriter::new(dir.path(),"s");
        writer.record_background_start("bash_9","nine",1.0,1.0).await;writer.record_background_start("bash_2","two",2.0,2.0).await;
        let state=crate::restore::parse_terminal_manifest(&writer.store.read().await.unwrap().unwrap(),"s").unwrap();assert_eq!(state.background_sessions.iter().map(|entry|entry.id.as_str()).collect::<Vec<_>>(),["bash_9","bash_2"]);
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn expired_and_ephemeral_store_entries_never_spawn_or_adopt() {
        let dir=tempfile::tempdir().unwrap();let mut writer=TerminalManifestWriter::new(dir.path(),"s");
        for (id,persistent) in [("mon_expired",true),("mon_ephemeral",false)] {
            writer.record_register(MonitorRegistration {monitor_id:id.to_owned(),spec:MonitorSpec::Command {description:id.to_owned(),command:"read value".to_owned(),filter:None,cwd:Some(dir.path().to_string_lossy().into_owned()),persistent}},1.0).await;
        }
        writer.record_background_start("bash_old","old",1.0,1.0).await;
        let mut next=TerminalManifestWriter::new(dir.path(),"s");let mut manager=crate::manager::TerminalManager::default();let mut registry=crate::monitor_registry::MonitorRegistry::new(|_|{});
        let digest=next.restore_live(&mut manager,&mut registry,1.0+DURABLE_MONITOR_EXPIRY_MS as f64).await;
        assert_eq!(digest,crate::restore::RestoreDigest {expired:1,lost:2,..Default::default()});assert_eq!(manager.size(),0);assert!(registry.snapshot().is_empty());assert_eq!(next.durable_count(),0);assert!(next.entries.is_empty());registry.dispose();
    }
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
    async fn configured_store_restore_delivers_real_shell_geometry() {
        let dir=tempfile::tempdir().unwrap();let mut writer=TerminalManifestWriter::new(dir.path(),"s");
        writer.record_register(MonitorRegistration {monitor_id:"mon_geometry".to_owned(),spec:MonitorSpec::Command {description:"geometry".to_owned(),command:"stty -echo; stty size; printf 'shell:%s\\n' \"${BASH_VERSION:+bash}\"".to_owned(),filter:None,cwd:Some(dir.path().to_string_lossy().into_owned()),persistent:true}},1.0).await;
        let mut next=TerminalManifestWriter::new(dir.path(),"s");let mut manager=crate::manager::TerminalManager::default();let (sender,mut events)=tokio::sync::mpsc::unbounded_channel();let mut registry=crate::monitor_registry::MonitorRegistry::new(move |event| {sender.send(event).unwrap();});let mut settings=crate::settings::TERMINAL_SETTINGS_DEFAULTS;settings.default_rows=33.0;settings.default_cols=91.0;
        assert_eq!(next.restore_configured_live(&mut manager,&mut registry,2.0,Some("/bin/bash"),&settings).await.restored,1);
        tokio::time::timeout(std::time::Duration::from_secs(5),async {for expected in ["33 91","shell:bash"] {assert!(matches!(events.recv().await,Some(crate::monitor_registry::MonitorEvent::Line {line,..}) if line==expected));}assert!(matches!(events.recv().await,Some(crate::monitor_registry::MonitorEvent::Summary {..})));}).await.unwrap();registry.dispose();manager.teardown().unwrap();
    }
    #[tokio::test]
    async fn missing_stable_identity_is_an_error() {let dir=tempfile::tempdir().unwrap();let mut writer=TerminalManifestWriter::new(dir.path(),"s");assert!(writer.observe_monitor_state(&[crate::monitor_registry::MonitorSnapshotEntry {id:"bash_1".to_owned(),..Default::default()}],0.0).await.is_err());}
}
#[cfg(test)]
mod debounce_tests {
    use super::*;
    #[tokio::test]
    async fn drain_cancels_timer_and_persists_last_checkpoint() {
        let dir=tempfile::tempdir().unwrap();let writer=std::sync::Arc::new(tokio::sync::Mutex::new(TerminalManifestWriter::new(dir.path(),"s")));
        writer.lock().await.record_register(MonitorRegistration {monitor_id:"mon_1".to_owned(),spec:MonitorSpec::Command {description:"watch".to_owned(),command:"true".to_owned(),filter:None,cwd:Some("/tmp".to_owned()),persistent:true}},1.0).await;
        let mut debounce=CheckpointDebouncer::new(writer.clone());
        for size in [1.0,2.0] {debounce.schedule("mon_1",TerminalManifestCheckpoint {dev:1.0,ino:1.0,size,mtime_ms:1.0,digest:format!("{size}"),present:true}).await;}
        debounce.flush(2.0).await;assert!(debounce.task.is_none());
        let state=crate::restore::parse_terminal_manifest(&writer.lock().await.store.read().await.unwrap().unwrap(),"s").unwrap();assert_eq!(state.monitors[0].last_checkpoint.as_ref().unwrap().size,2.0);
    }
}
