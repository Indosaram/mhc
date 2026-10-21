use super::js_installer::EnvironmentError;
use std::{
    fs::{File, OpenOptions},
    path::Path,
};

/// An OS-owned root lock is released even if an installer host crashes. Never unlink it:
/// every process must lock the same inode, including waiters that already opened it.
pub struct InstallLock(File);
impl Drop for InstallLock {
    fn drop(&mut self) {
        if let Err(error) = self.0.unlock() {
            maho_ext_api::diagnostic(&format!("environment lock release failed: {error}"));
        }
    }
}
pub async fn with_root_lock(
    base: &Path,
    signal: &maho_ext_api::AbortSignal,
) -> Result<InstallLock, EnvironmentError> {
    super::no_links::assert_no_links_below(base, &base.join(".install.lock"))?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(base.join(".install.lock"))?;
    loop {
        if signal.is_aborted() {
            return Err(EnvironmentError::cancelled());
        }
        match file.try_lock() {
            Ok(()) => return Ok(InstallLock(file)),
            Err(std::fs::TryLockError::WouldBlock) => {}
            Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
        }
        tokio::select! {()=signal.cancelled()=>return Err(EnvironmentError::cancelled()),()=tokio::time::sleep(std::time::Duration::from_millis(500))=>{}}
    }
}
