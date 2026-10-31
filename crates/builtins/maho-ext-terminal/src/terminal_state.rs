use crate::terminal_manifest::TerminalManifestWriter;
use std::path::{Path, PathBuf};
#[derive(Debug, PartialEq)]
pub enum TerminalStateAdoption {
    Acquired,
    AttachedElsewhere { pid: f64 },
}
/// The lease this generation holds: its path, the pid it records and the v2 generation token.
#[derive(Clone, Debug, PartialEq)]
pub struct HeldLease {
    pub path: PathBuf,
    pub pid: f64,
    pub token: String,
}
pub struct TerminalStateLifecycle {
    lease: Option<HeldLease>,
    pub writer: TerminalManifestWriter,
}
impl TerminalStateLifecycle {
    pub fn new(session_dir: &Path, session_id: &str) -> Self {
        Self {
            lease: None,
            writer: TerminalManifestWriter::new(session_dir, session_id),
        }
    }
    pub fn is_owner(&self) -> bool {
        self.lease.is_some()
    }
    pub fn acquire(
        &mut self,
        dir: &Path,
        encoded_session_id: &str,
        pid: f64,
        started_at_ms: f64,
    ) -> std::io::Result<TerminalStateAdoption> {
        // Upstream `acquireTerminalLease` probes the holder's start instant, so a lease whose pid
        // now belongs to an unrelated, later-started process is reclaimed at once.
        let mut is_process_alive = crate::manifest_lease::probe_alive;
        let mut read_process_start_ms = crate::manifest_lease::read_process_start_ms;
        match crate::manifest_lease::acquire_terminal_lease_with(
            dir,
            encoded_session_id,
            pid,
            started_at_ms,
            &mut crate::manifest_lease::LeaseProbes {
                is_process_alive: &mut is_process_alive,
                read_process_start_ms: &mut read_process_start_ms,
            },
        )? {
            crate::manifest_lease::AcquireTerminalLeaseResult::Acquired { path, pid, token } => {
                self.lease = Some(HeldLease { path, pid, token });
                Ok(TerminalStateAdoption::Acquired)
            }
            crate::manifest_lease::AcquireTerminalLeaseResult::Held { holder } => {
                Ok(TerminalStateAdoption::AttachedElsewhere { pid: holder.pid })
            }
        }
    }
    /// Binds a lease the lease keeper acquired on this generation's behalf (upstream keeper
    /// `onTakeover` -> `own(lease)`).
    pub fn adopt_lease(&mut self, lease: HeldLease) {
        self.lease = Some(lease);
    }
    /// Upstream `stopPersistence` `retireLeaseToken(lease.token)`: the moment shutdown starts, this
    /// generation's token stops counting as a live same-pid holder, before any slow flush.
    pub fn retire_token(&self) {
        if let Some(lease) = &self.lease {
            crate::manifest_lease::retire_lease_token(&lease.token);
        }
    }
    pub async fn restore(
        &mut self,
        manager: &mut crate::manager::TerminalManager,
        registry: &mut crate::monitor_registry::MonitorRegistry,
        now: f64,
    ) -> crate::restore::RestoreDigest {
        if self.lease.is_none() {
            return crate::restore::RestoreDigest::default();
        }
        self.writer.restore_live(manager, registry, now).await
    }
    pub async fn restore_configured(
        &mut self,
        manager: &mut crate::manager::TerminalManager,
        registry: &mut crate::monitor_registry::MonitorRegistry,
        now: f64,
        shell: Option<&str>,
        settings: &crate::settings::ResolvedTerminalSettings,
    ) -> crate::restore::RestoreDigest {
        if self.lease.is_none() {
            return crate::restore::RestoreDigest::default();
        }
        self.writer
            .restore_configured_live(manager, registry, now, shell, settings)
            .await
    }
    /// Reads the persisted manifest with no manager/registry locks held.
    pub async fn read_restore_state(
        &self,
    ) -> std::result::Result<Option<crate::terminal_manifest_model::TerminalManifest>, ()> {
        if self.lease.is_none() {
            return Ok(None);
        }
        self.writer.read_restore_state().await
    }
    /// Applies an already-read manifest synchronously, so the caller can hold the manager/registry
    /// mutexes only across this non-awaiting call.
    pub fn apply_restore(
        &mut self,
        state: std::result::Result<Option<crate::terminal_manifest_model::TerminalManifest>, ()>,
        manager: &mut crate::manager::TerminalManager,
        registry: &mut crate::monitor_registry::MonitorRegistry,
        now: f64,
        shell: Option<&str>,
        settings: &crate::settings::ResolvedTerminalSettings,
    ) -> crate::restore::RestoreDigest {
        if self.lease.is_none() {
            return crate::restore::RestoreDigest::default();
        }
        self.writer
            .apply_restore_state(state, manager, registry, now, shell, settings)
    }
    /// Records the manifest on shutdown. Gated on the lifecycle existing (this generation is the
    /// session's live recorder - acquired, or inherited across a reload), NOT on lease ownership: a
    /// reload generation inherits the lease without re-acquiring it yet still records, matching
    /// upstream `suspendAndFlushManifest`, which runs whenever the manifest writer is bound.
    pub async fn record_shutdown(&mut self, now: f64) {
        self.writer.record_shutdown(now).await;
    }
    pub fn release(&mut self) -> std::io::Result<()> {
        if let Some(lease) = self.lease.take() {
            crate::manifest_lease::release_terminal_lease(
                &lease.path,
                lease.pid,
                Some(lease.token.as_str()),
            )?;
        }
        Ok(())
    }
}
/// Releases a lease this generation inherited instead of acquiring (a reload generation keeps the
/// previous generation's lease). Upstream retires this process's current token for the session
/// first, then releases by (path, own pid, token): exactly that lease and no foreign holder's.
pub fn release_lease_at(dir: &Path, encoded_session_id: &str, pid: f64) -> std::io::Result<()> {
    let token = crate::manifest_lease::current_lease_token(encoded_session_id);
    if let Some(token) = &token {
        crate::manifest_lease::retire_lease_token(token);
    }
    crate::manifest_lease::release_terminal_lease(
        &dir.join(format!("{encoded_session_id}.lease")),
        pid,
        token.as_deref(),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn lease_gates_restore_and_release_transfers_ownership() -> std::io::Result<()> {
        let dir = tempfile::tempdir()?;
        let tmp = std::env::temp_dir();
        let tmp_str = tmp.to_str().unwrap().to_owned();
        let mut writer = TerminalManifestWriter::new(dir.path(), "s");
        writer
            .record_register(
                crate::terminal_manifest_model::MonitorRegistration {
                    monitor_id: "mon_saved".to_owned(),
                    spec: crate::terminal_manifest_model::MonitorSpec::Command {
                        description: "watch".to_owned(),
                        command: "stty -echo; printf 'restored\\n'".to_owned(),
                        filter: None,
                        human_input: None,
                        wakes_per_hour: None,
                        cwd: Some(tmp_str),
                        persistent: true,
                    },
                },
                1.0,
            )
            .await;
        writer.record_shutdown(2.0).await;

        // The acquisition instant is real: the start probe compares it with this process's start.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0.0, |elapsed| elapsed.as_secs_f64() * 1000.0);
        let mut owner = TerminalStateLifecycle::new(dir.path(), "s");
        assert_eq!(
            owner.acquire(dir.path(), "s", f64::from(std::process::id()), now)?,
            TerminalStateAdoption::Acquired
        );
        assert!(owner.is_owner());
        let mut manager = crate::manager::TerminalManager::default();
        let mut registry = crate::monitor_registry::MonitorRegistry::new(|_| {});
        assert_eq!(
            owner
                .restore(&mut manager, &mut registry, 20.0)
                .await
                .restored,
            1
        );
        assert_eq!(manager.resolve_id("mon_saved"), Some("bash_1".to_owned()));
        owner.record_shutdown(30.0).await;

        let mut other = TerminalStateLifecycle::new(dir.path(), "s");
        assert_eq!(
            other.acquire(dir.path(), "s", f64::from(std::process::id()) + 1.0, now)?,
            TerminalStateAdoption::AttachedElsewhere {
                pid: f64::from(std::process::id())
            }
        );
        assert!(!other.is_owner());
        assert_eq!(
            other.restore(&mut manager, &mut registry, 45.0).await,
            crate::restore::RestoreDigest::default()
        );

        owner.release()?;
        assert_eq!(
            other.acquire(dir.path(), "s", f64::from(std::process::id()) + 1.0, now)?,
            TerminalStateAdoption::Acquired
        );
        other.release()?;
        registry.dispose();
        manager.teardown().unwrap();
        Ok(())
    }

    /// terminal-restore-session.test.ts (e): two acquirers racing for one session - released at the
    /// same instant by a barrier - leave exactly one owner. Both record this live process, so the
    /// loser sees a live holder and never reclaims the winner's fresh lease.
    #[test]
    fn two_racing_acquirers_leave_exactly_one_owner() -> std::io::Result<()> {
        let dir = tempfile::tempdir()?;
        let terminal_dir = dir.path().join("extensions/terminal");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0.0, |elapsed| elapsed.as_secs_f64() * 1000.0);
        let barrier = &std::sync::Barrier::new(2);
        let (session_dir, terminal_dir) = (dir.path(), &terminal_dir);
        let outcomes = std::thread::scope(|scope| {
            let racers: Vec<_> = (0..2)
                .map(|_| {
                    scope.spawn(move || {
                        let mut lifecycle = TerminalStateLifecycle::new(session_dir, "s");
                        barrier.wait();
                        lifecycle
                            .acquire(terminal_dir, "s", f64::from(std::process::id()), now)
                            .map(|adoption| (adoption, lifecycle.is_owner()))
                    })
                })
                .collect();
            racers
                .into_iter()
                .map(|racer| racer.join().expect("racer thread"))
                .collect::<std::io::Result<Vec<_>>>()
        })?;
        assert_eq!(
            outcomes
                .iter()
                .filter(|(adoption, owner)| *adoption == TerminalStateAdoption::Acquired && *owner)
                .count(),
            1,
            "exactly one acquirer owns the lease: {outcomes:?}"
        );
        assert!(outcomes.iter().any(|(adoption, owner)| matches!(
            adoption,
            TerminalStateAdoption::AttachedElsewhere { .. }
        ) && !owner));
        Ok(())
    }
}
