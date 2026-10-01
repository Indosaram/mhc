use std::io::Write;
use std::path::{Path,PathBuf};
use crate::trust::HookTrustStorageScope;
use crate::trust_state_json::{empty_hook_trust_state,parse_hook_trust_state_json,read_hook_trust_state_json};
use crate::types::HookTrustState;

#[derive(Default)]
pub struct InMemoryHookStateStorage {global:Option<HookTrustState>,project:Option<HookTrustState>}
impl InMemoryHookStateStorage {
    pub fn read(&self,scope:HookTrustStorageScope)->HookTrustState {match scope {HookTrustStorageScope::Global=>&self.global,HookTrustStorageScope::Project=>&self.project}.clone().unwrap_or_else(empty_hook_trust_state)}
    pub fn update(&mut self,scope:HookTrustStorageScope,updater:impl FnOnce(HookTrustState)->HookTrustState)->HookTrustState {
        let next=updater(self.read(scope));match scope {HookTrustStorageScope::Global=>self.global=Some(next.clone()),HookTrustStorageScope::Project=>self.project=Some(next.clone())};next
    }
}

pub struct FileHookStateStorage {global_path:PathBuf,project_path:PathBuf}
impl FileHookStateStorage {
    pub fn new(agent_dir:&Path,cwd:&Path)->Self {Self {global_path:agent_dir.join("hooks-state.json"),project_path:cwd.join(".maho/hooks-state.json")}}
    fn path(&self,scope:HookTrustStorageScope)->&Path {match scope {HookTrustStorageScope::Global=>&self.global_path,HookTrustStorageScope::Project=>&self.project_path}}
    pub async fn read_async(&self,scope:HookTrustStorageScope)->std::io::Result<HookTrustState> {
        match tokio::fs::read_to_string(self.path(scope)).await {Ok(text)=>Ok(read_hook_trust_state_json(Some(&text))),Err(error) if error.kind()==std::io::ErrorKind::NotFound=>Ok(empty_hook_trust_state()),Err(error)=>Err(error)}
    }
    pub fn read(&self,scope:HookTrustStorageScope)->std::io::Result<HookTrustState> {
        let path=self.path(scope);let text=read_snapshot(path)?;
        if let Some(snapshot)=parse_hook_trust_state_json(text.as_deref()) {return Ok(snapshot);}
        let lease=match DirectoryLease::acquire(path) {
            Ok(lease)=>lease,
            Err(error) if matches!(error.kind(),std::io::ErrorKind::AlreadyExists|std::io::ErrorKind::PermissionDenied)||error.raw_os_error()==Some(30)=>return Ok(empty_hook_trust_state()),
            Err(error)=>return Err(error),
        };
        let result=read_snapshot(path).map(|text|read_hook_trust_state_json(text.as_deref()));
        release_result(lease,result)
    }
    pub fn update(&self,scope:HookTrustStorageScope,updater:impl FnOnce(HookTrustState)->HookTrustState)->std::io::Result<HookTrustState> {
        let path=self.path(scope);let lease=DirectoryLease::acquire(path)?;
        let result=(|| {
            let text=read_snapshot(path)?;let current=read_hook_trust_state_json(text.as_deref());
            #[cfg(unix)]
            let mode={use std::os::unix::fs::PermissionsExt;match std::fs::metadata(path) {Ok(metadata)=>metadata.permissions().mode()&0o777,Err(error) if error.kind()==std::io::ErrorKind::NotFound=>0o600,Err(error)=>return Err(error)}};
            let next=updater(current);let parent=path.parent().ok_or_else(||std::io::Error::other("hook state path has no parent"))?;
            let mut file=tempfile::Builder::new().prefix("hooks-state.").suffix(".tmp").tempfile_in(parent)?;
            #[cfg(unix)]
            {use std::os::unix::fs::PermissionsExt;file.as_file().set_permissions(std::fs::Permissions::from_mode(mode))?;}
            let mut serialized_state=next.clone();serialized_state.version=1;
            let mut serialized=serde_json::to_string_pretty(&serialized_state).map_err(std::io::Error::other)?;serialized.push('\n');file.write_all(serialized.as_bytes())?;
            file.persist(path).map_err(|error|error.error)?;Ok(next)
        })();release_result(lease,result)
    }
}

fn read_snapshot(path:&Path)->std::io::Result<Option<String>> {match std::fs::read_to_string(path) {Ok(text)=>Ok(Some(text)),Err(error) if error.kind()==std::io::ErrorKind::NotFound=>Ok(None),Err(error)=>Err(error)}}
struct DirectoryLease {path:PathBuf,held:bool}
impl DirectoryLease {
    fn acquire(path:&Path)->std::io::Result<Self> {
        std::fs::create_dir_all(path.parent().ok_or_else(||std::io::Error::other("hook state path has no parent"))?)?;
        let path=PathBuf::from(format!("{}.lock",path.display()));
        retry_lock(||acquire_directory(&path,true),||std::thread::sleep(std::time::Duration::from_millis(20)))?;
        Ok(Self {path,held:true})
    }
    fn release(mut self)->std::io::Result<()> {std::fs::remove_dir(&self.path)?;self.held=false;Ok(())}
}
fn retry_lock(mut acquire:impl FnMut()->std::io::Result<()>,mut delay:impl FnMut())->std::io::Result<()> {
    for attempt in 1..=10 {match acquire() {Err(error) if error.kind()==std::io::ErrorKind::AlreadyExists&&attempt<10=>delay(),result=>return result}}
    unreachable!("ten attempts always return")
}
fn acquire_directory(path:&Path,check_stale:bool)->std::io::Result<()> {
    match std::fs::create_dir(path) {
        Err(error) if error.kind()==std::io::ErrorKind::AlreadyExists&&check_stale=>{
            let metadata=match std::fs::metadata(path) {Err(error) if error.kind()==std::io::ErrorKind::NotFound=>return acquire_directory(path,false),result=>result?};
            let stale=metadata.modified()?<std::time::SystemTime::now()-std::time::Duration::from_secs(10);
            if !stale {return Err(error);}
            match std::fs::remove_dir(path) {Err(error) if error.kind()==std::io::ErrorKind::NotFound=>{},result=>result?}
            acquire_directory(path,false)
        },result=>result,
    }
}
impl Drop for DirectoryLease {fn drop(&mut self) {if self.held && let Err(error)=std::fs::remove_dir(&self.path) {eprintln!("hook state lock release failed: {error}");}}}
fn release_result<T>(lease:DirectoryLease,result:std::io::Result<T>)->std::io::Result<T> {
    match (result,lease.release()) {(Ok(value),Ok(()))=>Ok(value),(Err(error),Ok(()))=>Err(error),(Ok(_),Err(error))=>Err(error),(Err(operation),Err(release))=>Err(std::io::Error::other(format!("Hook state operation and lock release both failed: {operation}; {release}")))}
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn contention_retries_ten_attempts_but_other_errors_return_immediately() {
        let attempts=std::cell::Cell::new(0);let delays=std::cell::Cell::new(0);
        let error=retry_lock(|| {attempts.set(attempts.get()+1);Err(std::io::ErrorKind::AlreadyExists.into())},||delays.set(delays.get()+1)).unwrap_err();assert_eq!(error.kind(),std::io::ErrorKind::AlreadyExists);assert_eq!(attempts.get(),10);assert_eq!(delays.get(),9);
        attempts.set(0);delays.set(0);retry_lock(|| {attempts.set(attempts.get()+1);if attempts.get()<3 {Err(std::io::ErrorKind::AlreadyExists.into())} else {Ok(())}},||delays.set(delays.get()+1)).unwrap();assert_eq!(attempts.get(),3);assert_eq!(delays.get(),2);
        retry_lock(||Err(std::io::ErrorKind::PermissionDenied.into()),||panic!("non-contention errors must not retry")).unwrap_err();
    }
    #[test]
    fn stale_lock_directory_is_reclaimed_without_waiting()->std::io::Result<()> {
        let dir=tempfile::tempdir()?;let path=dir.path().join("state");let lock=dir.path().join("state.lock");std::fs::create_dir(&lock)?;
        std::fs::File::open(&lock)?.set_times(std::fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH))?;let lease=DirectoryLease::acquire(&path)?;assert!(lock.is_dir());lease.release()?;assert!(!lock.exists());Ok(())
    }
    #[tokio::test]
    async fn asynchronous_missing_and_invalid_snapshots_do_not_create_locks()->std::io::Result<()> {
        let dir=tempfile::tempdir()?;let storage=FileHookStateStorage::new(dir.path(),dir.path());assert_eq!(storage.read_async(HookTrustStorageScope::Global).await?,empty_hook_trust_state());assert!(!dir.path().join("hooks-state.json.lock").exists());
        std::fs::write(dir.path().join("hooks-state.json"),"invalid")?;let lease=DirectoryLease::acquire(&dir.path().join("hooks-state.json"))?;assert_eq!(storage.read_async(HookTrustStorageScope::Global).await?,empty_hook_trust_state());lease.release()
    }
    #[test] fn memory_scopes_remain_independent() {let mut storage=InMemoryHookStateStorage::default();storage.update(HookTrustStorageScope::Global,|mut state| {state.version=2;state});assert_eq!(storage.read(HookTrustStorageScope::Global).version,2);assert_eq!(storage.read(HookTrustStorageScope::Project).version,1);}
    #[test] fn publishes_snapshot_and_releases_lock()->std::io::Result<()> {let dir=tempfile::tempdir()?;let storage=FileHookStateStorage::new(dir.path(),dir.path());assert_eq!(storage.read(HookTrustStorageScope::Global)?,empty_hook_trust_state());storage.update(HookTrustStorageScope::Global,|state|state)?;assert!(dir.path().join("hooks-state.json").is_file());assert!(!dir.path().join("hooks-state.json.lock").exists());Ok(())}
    #[test] fn valid_snapshot_read_ignores_writer_lock()->std::io::Result<()> {let dir=tempfile::tempdir()?;let storage=FileHookStateStorage::new(dir.path(),dir.path());storage.update(HookTrustStorageScope::Global,|state|state)?;let lease=DirectoryLease::acquire(&dir.path().join("hooks-state.json"))?;assert_eq!(storage.read(HookTrustStorageScope::Global)?,empty_hook_trust_state());assert!(storage.update(HookTrustStorageScope::Global,|state|state).is_err());lease.release()}
    #[cfg(unix)]
    #[test] fn new_file_is_private_existing_mode_preserved()->std::io::Result<()> {use std::os::unix::fs::PermissionsExt;let dir=tempfile::tempdir()?;let storage=FileHookStateStorage::new(dir.path(),dir.path());let path=dir.path().join("hooks-state.json");storage.update(HookTrustStorageScope::Global,|state|state)?;assert_eq!(std::fs::metadata(&path)?.permissions().mode()&0o777,0o600);std::fs::set_permissions(&path,std::fs::Permissions::from_mode(0o644))?;storage.update(HookTrustStorageScope::Global,|state|state)?;assert_eq!(std::fs::metadata(&path)?.permissions().mode()&0o777,0o644);Ok(())}
}
