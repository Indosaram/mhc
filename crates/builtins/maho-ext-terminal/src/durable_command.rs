use crate::{manager::TerminalManager,monitor_registry::{CommandMonitor,MonitorSnapshotEntry},restore::RestoreOutcome,terminal_manifest_model::{ManifestMonitor,MonitorRuntimeKind}};
use maho_pty::PtySessionOptions;
use std::path::Path;

pub fn restore_command(monitor:&ManifestMonitor,manager:&mut TerminalManager,mut register:impl FnMut(&str,&crate::runtime_session::TerminalRuntimeSession,CommandMonitor)->Result<(),crate::runtime_session::RuntimeError>)->RestoreOutcome {
    if monitor.runtime_kind!=MonitorRuntimeKind::Command||!monitor.persistent {return RestoreOutcome::Lost;}
    let Some(command)=monitor.command.as_deref().filter(|command|!command.is_empty()) else {return RestoreOutcome::Lost;};
    let Some(cwd)=monitor.cwd.as_deref().filter(|cwd|Path::new(cwd).is_absolute()&&Path::new(cwd).is_dir()) else {return RestoreOutcome::Lost;};
    let options=PtySessionOptions::new("/bin/sh").arg("-c").arg(command).cwd(cwd);
    let Ok(id)=manager.create(command,options) else {return RestoreOutcome::Lost;};
    let mut record=CommandMonitor::new(MonitorSnapshotEntry {id:id.clone(),monitor_id:Some(monitor.monitor_id.clone()),description:monitor.description.clone(),command:monitor.command.clone(),filter:monitor.filter.clone(),persistent:Some(true),deadline_ms:None,expires_at:monitor.expires_at,..Default::default()},monitor.filter.as_deref().and_then(crate::shared::safe_reg_exp));
    if monitor.delivery_paused {record.pause();}
    manager.bind_monitor_id(&monitor.monitor_id,&id);
    if register(&id,manager.get(&id).expect("restored runtime"),record).is_err() {return RestoreOutcome::Lost;}
    if monitor.delivery_paused {RestoreOutcome::Muted} else {RestoreOutcome::Restored}
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn monitor()->ManifestMonitor {crate::restore::parse_terminal_manifest(&json!({"monitors":[{"monitorId":"mon_saved","sessionId":"s","description":"watch","runtimeKind":"command","durabilityClass":"restartable-command","command":"stty -echo; printf 'restored\\n'","cwd":"/tmp","createdAt":1,"expiresAt":null,"persistent":true,"suspended":false,"lastCheckpoint":null,"deliveryPaused":true,"fireWindow":{"startMs":1,"count":0}}],"backgroundSessions":[],"updatedAt":1}),"s").unwrap().monitors.remove(0)}
    #[test]
    fn restores_once_with_stable_identity_and_fresh_muted_runtime() {
        let saved=monitor();let mut manager=TerminalManager::default();let mut calls=0;
        assert_eq!(restore_command(&saved,&mut manager,|id,_,record| {calls+=1;assert_eq!(id,"bash_1");assert!(record.snapshot.paused);assert_eq!(record.snapshot.deadline_ms,None);Ok(())}),RestoreOutcome::Muted);
        assert_eq!(calls,1);assert_eq!(manager.resolve_id("mon_saved"),Some("bash_1".to_owned()));
        let runtime=manager.get("bash_1").unwrap();runtime.wait(std::time::Duration::from_secs(5)).unwrap();assert_eq!(runtime.full_output().unwrap(),"restored\r\n");manager.teardown().unwrap();
    }
    #[test]
    fn invalid_targets_do_not_spawn() {
        let mut saved=monitor();let mut manager=TerminalManager::default();saved.cwd=Some("relative".to_owned());
        assert_eq!(restore_command(&saved,&mut manager,|_,_,_|panic!("invalid target registered")),RestoreOutcome::Lost);assert_eq!(manager.size(),0);
        saved.cwd=Some("/tmp".to_owned());saved.persistent=false;
        assert_eq!(restore_command(&saved,&mut manager,|_,_,_|panic!("ephemeral target registered")),RestoreOutcome::Lost);assert_eq!(manager.size(),0);
    }
    #[tokio::test]
    async fn restored_runtime_is_observed_by_live_registry() {
        let mut saved=monitor();saved.delivery_paused=false;
        let (sender,mut events)=tokio::sync::mpsc::unbounded_channel();let mut registry=crate::monitor_registry::MonitorRegistry::new(move |event| {sender.send(event).unwrap();});let mut manager=TerminalManager::default();
        assert_eq!(restore_command(&saved,&mut manager,|_,runtime,record|registry.register(runtime,record)),RestoreOutcome::Restored);
        tokio::time::timeout(std::time::Duration::from_secs(5),async {assert!(matches!(events.recv().await,Some(crate::monitor_registry::MonitorEvent::Line {line,..}) if line=="restored"));assert!(matches!(events.recv().await,Some(crate::monitor_registry::MonitorEvent::Summary {..})));}).await.unwrap();manager.teardown().unwrap();
    }
}
