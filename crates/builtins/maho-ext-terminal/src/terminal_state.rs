use std::path::{Path,PathBuf};
use crate::terminal_manifest::TerminalManifestWriter;
#[derive(Debug,PartialEq)]
pub enum TerminalStateAdoption {Acquired,AttachedElsewhere {pid:f64}}
pub struct TerminalStateLifecycle {lease:Option<(PathBuf,f64)>,pub writer:TerminalManifestWriter}
impl TerminalStateLifecycle {
    pub fn new(session_dir:&Path,session_id:&str)->Self {Self {lease:None,writer:TerminalManifestWriter::new(session_dir,session_id)}}
    pub fn is_owner(&self)->bool {self.lease.is_some()}
    #[cfg(unix)]
    pub fn acquire(&mut self,dir:&Path,encoded_session_id:&str,pid:f64,started_at_ms:f64)->std::io::Result<TerminalStateAdoption> {
        match crate::manifest_lease::acquire_terminal_lease(dir,encoded_session_id,pid,started_at_ms,crate::manifest_lease::probe_alive)? {
            crate::manifest_lease::AcquireTerminalLeaseResult::Acquired {path,pid}=>{self.lease=Some((path,pid));Ok(TerminalStateAdoption::Acquired)},
            crate::manifest_lease::AcquireTerminalLeaseResult::Held {holder}=>Ok(TerminalStateAdoption::AttachedElsewhere {pid:holder.pid}),
        }
    }
    #[cfg(unix)]
    pub async fn restore(&mut self,manager:&mut crate::manager::TerminalManager,registry:&mut crate::monitor_registry::MonitorRegistry,now:f64)->crate::restore::RestoreDigest {
        if self.lease.is_none() {return crate::restore::RestoreDigest::default();}
        self.writer.restore_live(manager,registry,now).await
    }
    #[cfg(unix)]
    pub async fn restore_configured(&mut self,manager:&mut crate::manager::TerminalManager,registry:&mut crate::monitor_registry::MonitorRegistry,now:f64,shell:Option<&str>,settings:&crate::settings::ResolvedTerminalSettings)->crate::restore::RestoreDigest {
        if self.lease.is_none() {return crate::restore::RestoreDigest::default();}
        self.writer.restore_configured_live(manager,registry,now,shell,settings).await
    }
    /// Reads the persisted manifest with no manager/registry locks held.
    #[cfg(unix)]
    pub async fn read_restore_state(&self) -> std::result::Result<Option<crate::terminal_manifest_model::TerminalManifest>,()> {
        if self.lease.is_none() {return Ok(None);}
        self.writer.read_restore_state().await
    }
    /// Applies an already-read manifest synchronously, so the caller can hold the manager/registry
    /// mutexes only across this non-awaiting call.
    #[cfg(unix)]
    pub fn apply_restore(&mut self,state:std::result::Result<Option<crate::terminal_manifest_model::TerminalManifest>,()>,manager:&mut crate::manager::TerminalManager,registry:&mut crate::monitor_registry::MonitorRegistry,now:f64,shell:Option<&str>,settings:&crate::settings::ResolvedTerminalSettings)->crate::restore::RestoreDigest {
        if self.lease.is_none() {return crate::restore::RestoreDigest::default();}
        self.writer.apply_restore_state(state,manager,registry,now,shell,settings)
    }
    pub async fn record_shutdown(&mut self,now:f64) {if self.lease.is_some() {self.writer.record_shutdown(now).await;}}
    #[cfg(unix)]
    pub fn release(&mut self)->std::io::Result<()> {if let Some((path,pid))=self.lease.take() {crate::manifest_lease::release_terminal_lease(&path,pid)?;}Ok(())}
}
/// Releases a lease this generation inherited instead of acquiring (a reload generation keeps the
/// previous generation's lease). `release_terminal_lease` unlinks only when the file records `pid`,
/// so this removes exactly that lease and no foreign holder's.
#[cfg(unix)]
pub fn release_lease_at(dir:&Path,encoded_session_id:&str,pid:f64)->std::io::Result<()> {
    crate::manifest_lease::release_terminal_lease(&dir.join(format!("{encoded_session_id}.lease")),pid)
}
#[cfg(all(test,unix))]
mod tests {
    use super::*;
    #[tokio::test]
    async fn lease_gates_restore_and_release_transfers_ownership() -> std::io::Result<()> {
        let dir=tempfile::tempdir()?;
        let mut writer=TerminalManifestWriter::new(dir.path(),"s");
        writer.record_register(crate::terminal_manifest_model::MonitorRegistration {monitor_id:"mon_saved".to_owned(),spec:crate::terminal_manifest_model::MonitorSpec::Command {description:"watch".to_owned(),command:"stty -echo; printf 'restored\\n'".to_owned(),filter:None,cwd:Some("/tmp".to_owned()),persistent:true}},1.0).await;
        writer.record_shutdown(2.0).await;

        let mut owner=TerminalStateLifecycle::new(dir.path(),"s");
        assert_eq!(owner.acquire(dir.path(),"s",f64::from(std::process::id()),10.0)?,TerminalStateAdoption::Acquired);
        assert!(owner.is_owner());
        let mut manager=crate::manager::TerminalManager::default();let mut registry=crate::monitor_registry::MonitorRegistry::new(|_|{});
        assert_eq!(owner.restore(&mut manager,&mut registry,20.0).await.restored,1);
        assert_eq!(manager.resolve_id("mon_saved"),Some("bash_1".to_owned()));
        owner.record_shutdown(30.0).await;

        let mut other=TerminalStateLifecycle::new(dir.path(),"s");
        assert_eq!(other.acquire(dir.path(),"s",f64::from(std::process::id())+1,40.0)?,TerminalStateAdoption::AttachedElsewhere {pid:f64::from(std::process::id())});
        assert!(!other.is_owner());
        assert_eq!(other.restore(&mut manager,&mut registry,45.0).await,crate::restore::RestoreDigest::default());

        owner.release()?;
        assert_eq!(other.acquire(dir.path(),"s",f64::from(std::process::id())+1,50.0)?,TerminalStateAdoption::Acquired);
        other.release()?;registry.dispose();manager.teardown().unwrap();Ok(())
    }
}