use crate::terminal_manifest_model::{TerminalManifestCheckpoint,ManifestMonitor};
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum DetachedChange {Created,Replaced,Modified,Gone}
pub fn classify_detached_change(saved:&TerminalManifestCheckpoint,live:&TerminalManifestCheckpoint)->Option<DetachedChange> {
    if !saved.present {return if live.present {Some(DetachedChange::Created)} else {None};}
    if !live.present {return Some(DetachedChange::Gone);}
    if live.dev!=saved.dev||live.ino!=saved.ino {return Some(DetachedChange::Replaced);}
    if live.mtime_ms!=saved.mtime_ms||live.size!=saved.size||live.digest!=saved.digest {return Some(DetachedChange::Modified);}
    None
}
pub fn remaining_ms(monitor:&ManifestMonitor,now:f64)->f64 {monitor.expires_at.map_or(300_000.0,|expiry|(expiry-now).max(1.0))}
#[cfg(unix)]
pub fn restore_file(monitor:&ManifestMonitor,registry:&mut crate::monitor_registry::MonitorRegistry,manager:&mut crate::manager::TerminalManager,writer:Option<&mut crate::terminal_manifest::TerminalManifestWriter>,now:f64)->crate::restore::RestoreOutcome {
    use crate::{restore::RestoreOutcome,terminal_manifest_model::MonitorRuntimeKind};
    if monitor.runtime_kind!=MonitorRuntimeKind::File {return RestoreOutcome::Lost;}
    let (Some(path),Some(cwd),Some(saved))=(&monitor.path,&monitor.cwd,&monitor.last_checkpoint) else {return RestoreOutcome::Lost;};
    let target=std::path::Path::new(cwd).join(path);
    if saved.present&&!target.is_file() {return RestoreOutcome::Lost;}
    let Ok(Some(reservation))=manager.reserve() else {return RestoreOutcome::Lost;};
    let registered=registry.restore_persistent_file(monitor,&target,now);
    let Ok((id,_))=registered else {return RestoreOutcome::Lost;};
    registry.reserve_file_capacity(&id,reservation);
    let Some(live)=registry.file_checkpoint(&id) else {return RestoreOutcome::Lost;};
    let change=classify_detached_change(saved,&live);
    if change==Some(DetachedChange::Gone) {registry.stop_file(&id);return RestoreOutcome::Lost;}
    manager.bind_monitor_id(&monitor.monitor_id,&id);
    if let Some(change)=change {
        let change=match change {DetachedChange::Created=>"created",DetachedChange::Replaced=>"replaced",DetachedChange::Modified=>"modified",DetachedChange::Gone=>unreachable!()};
        registry.emit_file_line(&id,format!("changed while detached: {change} {path}"));
    }
    if let Some(writer)=writer {writer.schedule_checkpoint(&monitor.monitor_id,live);}
    RestoreOutcome::Restored
}
#[cfg(unix)]
pub fn file_checkpoint(path:&std::path::Path)->std::io::Result<TerminalManifestCheckpoint> {
    file_checkpoint_with_identity(path,None)
}
#[cfg(unix)]
pub fn file_checkpoint_with_identity(path:&std::path::Path,identity:Option<(f64,f64)>)->std::io::Result<TerminalManifestCheckpoint> {
    use std::os::unix::fs::MetadataExt;
    let absent=||TerminalManifestCheckpoint {dev:0.0,ino:0.0,size:0.0,mtime_ms:0.0,digest:String::new(),present:false};
    let initial=match std::fs::symlink_metadata(path) {Ok(metadata)=>metadata,Err(error) if error.kind()==std::io::ErrorKind::NotFound=>return Ok(absent()),Err(error)=>return Err(error)};
    if initial.file_type().is_symlink() {return Err(std::io::Error::other(format!("Cannot watch file: target is a symbolic link: {}",path.display())));}
    if !initial.is_file() {return Err(std::io::Error::other(format!("Cannot watch file: target is not a regular file: {}",path.display())));}
    let mut handle=std::fs::File::open(path)?;
    let opened=handle.metadata()?;
    if identity.is_some_and(|(dev,ino)|opened.dev() as f64!=dev||opened.ino() as f64!=ino)&&opened.nlink()>1 {return Err(std::io::Error::other(format!("Cannot watch file: target identity changed: {}",path.display())));}
    let rebound=std::fs::symlink_metadata(path)?;
    if !opened.is_file()||rebound.file_type().is_symlink()||rebound.dev()!=opened.dev()||rebound.ino()!=opened.ino() {return Err(std::io::Error::other(format!("Cannot watch file: target identity changed: {}",path.display())));}
    let digest=crate::monitor_file_digest::digest_file_handle(&mut handle)?;
    let after=std::fs::symlink_metadata(path)?;
    if after.file_type().is_symlink()||after.dev()!=opened.dev()||after.ino()!=opened.ino() {return Err(std::io::Error::other(format!("Cannot watch file: target identity changed: {}",path.display())));}
    Ok(TerminalManifestCheckpoint {dev:opened.dev() as f64,ino:opened.ino() as f64,size:opened.len() as f64,mtime_ms:opened.mtime() as f64*1000.0+opened.mtime_nsec() as f64/1_000_000.0,digest,present:true})
}
#[cfg(all(test,unix))]
mod filesystem_tests {
    use super::*;
    #[tokio::test]
    async fn restore_reports_detached_change_once_and_rebinds_kill_identity()->std::io::Result<()> {
        let dir=tempfile::tempdir()?;let path=dir.path().join("watched");std::fs::write(&path,b"old")?;let saved=file_checkpoint(&path)?;std::fs::write(&path,b"new")?;
        let mut manifest=crate::restore::parse_terminal_manifest(&serde_json::json!({"monitors":[{"monitorId":"mon_saved","sessionId":"s","description":"watch","runtimeKind":"file","durabilityClass":"checkpointed-file","path":"watched","cwd":dir.path().to_string_lossy(),"event":"modify","createdAt":1,"expiresAt":null,"persistent":true,"suspended":true,"lastCheckpoint":saved,"deliveryPaused":false,"fireWindow":{"startMs":1,"count":0}}],"backgroundSessions":[],"updatedAt":1}),"s").unwrap();
        let monitor=manifest.monitors.remove(0);let (sender,mut events)=tokio::sync::mpsc::unbounded_channel();let mut registry=crate::monitor_registry::MonitorRegistry::new(move |event| {sender.send(event).unwrap();});let mut manager=crate::manager::TerminalManager::default();
        assert_eq!(restore_file(&monitor,&mut registry,&mut manager,None,2.0),crate::restore::RestoreOutcome::Restored);
        assert_eq!(registry.snapshot()[0].expires_at,monitor.expires_at);assert_eq!(registry.snapshot()[0].persistent,Some(true));assert!(registry.snapshot()[0].deadline_ms.is_none());
        assert!(matches!(events.try_recv(),Ok(crate::monitor_registry::MonitorEvent::Line {line,..}) if line=="changed while detached: modified watched"));assert!(events.try_recv().is_err());
        let runtime=manager.resolve_id("mon_saved").unwrap();assert_eq!(runtime,"watch_1");assert_eq!(manager.active_size().unwrap(),1);assert!(registry.stop_file(&runtime));assert_eq!(manager.active_size().unwrap(),0);Ok(())
    }
    #[test]
    fn snapshot_hashes_regular_files_and_rejects_symlinks()->std::io::Result<()> {
        let dir=tempfile::tempdir()?;let path=dir.path().join("watched");
        assert!(!file_checkpoint(&path)?.present);
        std::fs::write(&path,b"old")?;let saved=file_checkpoint(&path)?;
        std::fs::write(&path,b"new")?;assert_eq!(classify_detached_change(&saved,&file_checkpoint(&path)?),Some(DetachedChange::Modified));
        let link=dir.path().join("link");std::os::unix::fs::symlink(&path,&link)?;assert!(file_checkpoint(&link).is_err());assert!(file_checkpoint(dir.path()).is_err());Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn detached_change_precedence_and_digest_only_modification() {let saved=TerminalManifestCheckpoint {dev:1.0,ino:2.0,size:4.0,mtime_ms:5.0,digest:"old".to_owned(),present:true};let mut live=saved.clone();assert_eq!(classify_detached_change(&saved,&live),None);live.digest="new".to_owned();assert_eq!(classify_detached_change(&saved,&live),Some(DetachedChange::Modified));live.ino=3.0;assert_eq!(classify_detached_change(&saved,&live),Some(DetachedChange::Replaced));live.present=false;assert_eq!(classify_detached_change(&saved,&live),Some(DetachedChange::Gone));let mut absent=saved;absent.present=false;assert_eq!(classify_detached_change(&absent,&live),None);live.present=true;assert_eq!(classify_detached_change(&absent,&live),Some(DetachedChange::Created));}
}
