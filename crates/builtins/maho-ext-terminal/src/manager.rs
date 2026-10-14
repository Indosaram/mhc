use std::collections::BTreeMap;
use std::time::Duration;
use maho_pty::PtySessionOptions;
use crate::runtime_session::{TerminalRuntimeSession,RuntimeError};
use crate::shared::DEFAULT_MAX_SESSIONS;
// registry.ts: DEFAULT_COMMAND / DEFAULT_STOP_EXIT_GRACE_MS / DEFAULT_FORCED_EXIT_GRACE_MS.
const DEFAULT_COMMAND:&str="bash";
const DEFAULT_STOP_EXIT_GRACE_MS:u64=5000;
const DEFAULT_FORCED_EXIT_GRACE_MS:u64=1000;
/// registry-session.ts `sessionIdPrefix`: the shell basename (Windows executable extension stripped),
/// lowercased with non-alphanumeric runs collapsed to `_`, so ids read `bash_N`, `sh_N`, `pwsh_N`, `cmd_N`.
pub fn session_id_prefix(command:&str)->String {
    let base_name=command.split(['\\','/']).filter(|part|!part.is_empty()).last().unwrap_or(DEFAULT_COMMAND);
    let lowered=base_name.to_lowercase();
    let without_ext=[".exe",".com",".bat",".cmd",".ps1"].iter().find_map(|ext|lowered.strip_suffix(ext)).unwrap_or(&lowered);
    let mut prefix=String::new();
    for character in without_ext.chars() {if character.is_ascii_lowercase()||character.is_ascii_digit() {prefix.push(character);} else if !prefix.ends_with('_') {prefix.push('_');}}
    let prefix=prefix.trim_matches('_');
    if prefix.is_empty() {"session".to_owned()} else {prefix.to_owned()}
}
/// Await the settled exit for at most `grace`; `false` means it did not settle in time.
fn wait_for_terminal_session_exit(runtime:&TerminalRuntimeSession,grace:Duration)->Result<bool,RuntimeError> {
    match runtime.wait(grace) {Err(RuntimeError::WaitTimeout)=>Ok(false),Err(RuntimeError::Poisoned)=>Err(RuntimeError::Poisoned),_=>runtime.exited()}
}
#[derive(Debug,thiserror::Error)]
pub enum ManagerError {#[error("Cannot create terminal session: capacity limit ({0}) reached.")] Capacity(usize),#[error(transparent)] Runtime(#[from] RuntimeError)}
struct Entry {runtime:TerminalRuntimeSession,created_at:u64,last_used_at:u64,stop_requested:bool,force_killed:bool}
pub struct TerminalManager {entries:BTreeMap<String,Entry>,monitor_ids:BTreeMap<String,String>,max_sessions:usize,scrollback:usize,id_counters:BTreeMap<String,u64>,sequence:u64,reservations:std::sync::Arc<std::sync::atomic::AtomicUsize>}
pub struct CapacityReservation(std::sync::Arc<std::sync::atomic::AtomicUsize>);
impl Drop for CapacityReservation {fn drop(&mut self) {self.0.fetch_sub(1,std::sync::atomic::Ordering::SeqCst);}}
impl Default for TerminalManager {fn default()->Self {Self::new(DEFAULT_MAX_SESSIONS)}}
impl TerminalManager {
    pub fn new(max_sessions:usize)->Self {Self {entries:BTreeMap::new(),monitor_ids:BTreeMap::new(),max_sessions,scrollback:crate::shared::DEFAULT_SCROLLBACK,id_counters:BTreeMap::new(),sequence:0,reservations:Default::default()}}
    pub fn configure(&mut self,settings:&crate::settings::ResolvedTerminalSettings) {self.max_sessions=settings.max_sessions as usize;self.scrollback=settings.scrollback as usize;}
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
        let prefix=session_id_prefix(&options.command);
        let runtime=TerminalRuntimeSession::start_with_scrollback(command,options,self.scrollback)?;let id=self.allocate_id(&prefix);self.sequence+=1;self.entries.insert(id.clone(),Entry {runtime,created_at:self.sequence,last_used_at:self.sequence,stop_requested:false,force_killed:false});Ok(id)
    }
    /// registry.ts `allocateId`: per-prefix counters, skipping any id still held by an entry.
    fn allocate_id(&mut self,prefix:&str)->String {
        let mut next=self.id_counters.get(prefix).copied().unwrap_or(0)+1;
        while self.entries.contains_key(&format!("{prefix}_{next}")) {next+=1;}
        self.id_counters.insert(prefix.to_owned(),next);format!("{prefix}_{next}")
    }
    pub fn get(&mut self,id:&str)->Option<&mut TerminalRuntimeSession> {self.sequence+=1;let entry=self.entries.get_mut(id)?;entry.last_used_at=self.sequence;Some(&mut entry.runtime)}
    pub fn bind_monitor_id(&mut self,monitor_id:&str,session_id:&str) {self.monitor_ids.insert(monitor_id.to_owned(),session_id.to_owned());}
    pub fn resolve_id(&self,id:&str)->Option<String> {if id.starts_with("mon_") {self.monitor_ids.get(id).cloned()} else if self.entries.contains_key(id) {Some(id.to_owned())} else {None}}
    pub fn list(&self)->Vec<(&str,&TerminalRuntimeSession)> {let mut entries=self.entries.iter().collect::<Vec<_>>();entries.sort_by_key(|(_,entry)|entry.created_at);entries.into_iter().map(|(id,entry)|(id.as_str(),&entry.runtime)).collect()}
    /// Tree-stop one session: SIGTERM, a bounded wait, then SIGKILL (registry.ts `stopEntry`). A
    /// session that outlives the forced signal stays registered as stopping; that is not an error.
    pub fn stop(&mut self,id:&str)->Result<bool,RuntimeError> {if !self.entries.contains_key(id) {return Ok(false);}self.stop_entries(&[id.to_owned()])?;Ok(true)}
    /// registry.ts `teardown`: stop every session concurrently, SIGKILL every survivor, then forget all
    /// entries even when one is stuck, so later sessions are still stopped and disposed.
    pub fn teardown(&mut self)->Result<(),RuntimeError> {
        let ids=self.entries.keys().cloned().collect::<Vec<_>>();
        let mut first_error=self.stop_entries(&ids).err();
        if let Err(error)=self.force_kill_entries(&ids) {first_error.get_or_insert(error);}
        for (_,entry) in std::mem::take(&mut self.entries) {if let Err(error)=entry.runtime.dispose() {first_error.get_or_insert(error);}}
        self.monitor_ids.clear();self.id_counters.clear();
        first_error.map_or(Ok(()),Err)
    }
    fn stop_entries(&mut self,ids:&[String])->Result<(),RuntimeError> {
        let mut first_error=None;let mut pending=Vec::new();
        for id in ids {
            let Some(entry)=self.entries.get_mut(id) else {continue;};
            if entry.stop_requested {continue;}entry.stop_requested=true;
            match entry.runtime.exited() {Ok(true)=>continue,Ok(false)=>{},Err(error)=>{first_error.get_or_insert(error);continue;}}
            match entry.runtime.stop() {Ok(())=>pending.push(id.clone()),Err(error)=>{first_error.get_or_insert(error);}}
        }
        match self.wait_entries_exit(pending,Duration::from_millis(DEFAULT_STOP_EXIT_GRACE_MS)) {
            Ok(survivors)=>if let Err(error)=self.force_kill_entries(&survivors) {first_error.get_or_insert(error);},
            Err(error)=>{first_error.get_or_insert(error);}
        }
        first_error.map_or(Ok(()),Err)
    }
    /// registry.ts `forceKillEntry`: SIGKILL once and wait the forced grace; a survivor stays `stopping`.
    fn force_kill_entries(&mut self,ids:&[String])->Result<(),RuntimeError> {
        let mut first_error=None;let mut pending=Vec::new();
        for id in ids {
            let Some(entry)=self.entries.get_mut(id) else {continue;};
            match entry.runtime.exited() {Ok(true)=>continue,Ok(false)=>{},Err(error)=>{first_error.get_or_insert(error);continue;}}
            if entry.force_killed {continue;}entry.force_killed=true;
            match entry.runtime.kill() {Ok(())=>pending.push(id.clone()),Err(error)=>{first_error.get_or_insert(error);}}
        }
        if let Err(error)=self.wait_entries_exit(pending,Duration::from_millis(DEFAULT_FORCED_EXIT_GRACE_MS)) {first_error.get_or_insert(error);}
        first_error.map_or(Ok(()),Err)
    }
    /// Wait for every id against one shared deadline (upstream awaits the stops concurrently).
    fn wait_entries_exit(&self,ids:Vec<String>,grace:Duration)->Result<Vec<String>,RuntimeError> {
        let deadline=std::time::Instant::now()+grace;let mut survivors=Vec::new();
        for id in ids {let Some(entry)=self.entries.get(&id) else {continue;};if !wait_for_terminal_session_exit(&entry.runtime,deadline.saturating_duration_since(std::time::Instant::now()))? {survivors.push(id);}}
        Ok(survivors)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn configured_capacity_and_zero_scrollback_reach_new_sessions()->Result<(),ManagerError> {
        let mut manager=TerminalManager::default();let mut settings=crate::settings::TERMINAL_SETTINGS_DEFAULTS;settings.max_sessions=1.0;settings.scrollback=0.0;manager.configure(&settings);
        let id=manager.create("read",PtySessionOptions::new("/bin/sh").arg("-c").arg("stty -echo; read start; printf 'one\\r\\ntwo\\r\\nthree'; read end").size(20,2))?;
        assert!(manager.reserve()?.is_none());let runtime=manager.get(&id).unwrap();let (_,mut output)=runtime.subscribe_output()?;runtime.write(b"start\n")?;
        tokio::time::timeout(Duration::from_secs(5),async {let mut text=String::new();while !text.contains("three") {text.push_str(&output.recv().await.unwrap());}}).await.unwrap();
        assert!(manager.get(&id).unwrap().snapshot()?.scrollback.is_empty());manager.teardown()?;Ok(())
    }
    #[test] fn reservation_counts_until_release()->Result<(),RuntimeError> {let mut manager=TerminalManager::new(1);let reservation=manager.reserve()?.expect("capacity");assert_eq!(manager.active_size()?,1);assert!(manager.reserve()?.is_none());drop(reservation);assert_eq!(manager.active_size()?,0);assert!(manager.reserve()?.is_some());Ok(())}
    #[test] fn capacity_stop_and_monitor_bindings()->Result<(),ManagerError> {let mut manager=TerminalManager::new(1);let id=manager.create("read",PtySessionOptions::new("/bin/sh").arg("-c").arg("read value"))?;manager.bind_monitor_id("mon_1",&id);assert_eq!(manager.resolve_id("mon_1"),Some(id.clone()));assert_eq!(manager.active_size()?,1);assert!(matches!(manager.create("read",PtySessionOptions::new("/bin/sh").arg("-c").arg("read value")),Err(ManagerError::Capacity(1))));assert!(manager.stop(&id)?);assert!(manager.get(&id).is_some());manager.teardown()?;assert_eq!(manager.size(),0);assert_eq!(manager.resolve_id("mon_1"),None);Ok(())}
    #[test] fn session_ids_use_the_shell_prefix_with_per_prefix_counters()->Result<(),ManagerError> {
        for (command,prefix) in [("/bin/bash","bash"),("C:\\Program Files\\Git\\bin\\bash.exe","bash"),("/bin/sh","sh"),("pwsh.exe","pwsh"),("cmd.exe","cmd"),("/usr/bin/my-Shell","my_shell"),("/","bash"),("--","session")] {assert_eq!(session_id_prefix(command),prefix);}
        let mut manager=TerminalManager::new(4);
        let first=manager.create("a",PtySessionOptions::new("/bin/sh").arg("-c").arg("true"))?;let second=manager.create("b",PtySessionOptions::new("/bin/sh").arg("-c").arg("true"))?;
        assert_eq!((first.as_str(),second.as_str()),("sh_1","sh_2"));manager.teardown()?;Ok(())
    }
    #[tokio::test]
    async fn stop_sends_sigterm_so_traps_clean_up_before_any_sigkill()->Result<(),ManagerError> {
        let mut manager=TerminalManager::new(1);
        let id=manager.create("trap",PtySessionOptions::new("/bin/sh").arg("-c").arg("stty -echo; trap 'printf cleaned; exit 7' TERM; printf 'ready\\n'; sleep 30 & wait"))?;
        let (history,mut output)=manager.get(&id).expect("session").subscribe_output()?;let mut observed=history;
        tokio::time::timeout(Duration::from_secs(5),async {while !observed.contains("ready") {observed.push_str(&output.recv().await.expect("output"));}}).await.expect("ready line");
        assert!(manager.stop(&id)?);
        let runtime=manager.get(&id).expect("session");let exit=runtime.exit_result()?.expect("settled exit");
        assert_eq!(exit.exit_code,Some(7));assert!(exit.cancelled);assert!(runtime.full_output()?.contains("cleaned"));manager.teardown()?;Ok(())
    }
    #[cfg(target_os="linux")]
    #[tokio::test]
    async fn teardown_continues_past_a_session_whose_pty_outlives_sigkill()->Result<(),ManagerError> {
        let mut manager=TerminalManager::new(2);
        let stuck=manager.create("stuck",PtySessionOptions::new("/bin/sh").arg("-c").arg("stty -echo; setsid sleep 20 & printf 'ready\\n'; wait"))?;
        let other=manager.create("other",PtySessionOptions::new("/bin/sh").arg("-c").arg("read value"))?;
        let (history,mut output)=manager.get(&stuck).expect("session").subscribe_output()?;let mut observed=history;
        tokio::time::timeout(Duration::from_secs(5),async {while !observed.contains("ready") {observed.push_str(&output.recv().await.expect("output"));}}).await.expect("ready line");
        let mut other_exit=manager.get(&other).expect("session").subscribe_exit();
        assert!(manager.stop(&stuck)?);assert!(!manager.get(&stuck).expect("stopping entry stays registered").exited()?);
        manager.teardown()?;assert_eq!(manager.size(),0);
        tokio::time::timeout(Duration::from_secs(5),async {while other_exit.borrow_and_update().is_none() {other_exit.changed().await.expect("exit sender");}}).await.expect("later session stopped");Ok(())
    }
}
