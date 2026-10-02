use std::{path::{Path,PathBuf},time::Duration};
#[derive(Debug,thiserror::Error)]pub enum LockError{
    #[error("Legacy lock directory is present at {0}")]LegacyLockArtifact(PathBuf),
    #[error(transparent)]Io(#[from]std::io::Error),
    #[error(transparent)]Sqlite(#[from]rusqlite::Error),
}
pub struct LockRetries{pub retries:u32,pub min_timeout:Duration,pub max_timeout:Duration}
impl Default for LockRetries{fn default()->Self{Self{retries:100,min_timeout:Duration::from_millis(20),max_timeout:Duration::from_millis(100)}}}
pub struct OwnershipSafeLock{database:Option<rusqlite::Connection>}
impl OwnershipSafeLock{pub fn release(&mut self)->Result<(),LockError>{let Some(database)=self.database.take()else{return Ok(());};let result=database.execute_batch("COMMIT;");drop(database);result.map_err(Into::into)}}
fn reject_legacy_directory(path:&Path)->Result<(),LockError>{match std::fs::metadata(path){Ok(metadata)if metadata.is_dir()=>Err(LockError::LegacyLockArtifact(path.into())),Ok(_)=>Ok(()),Err(error)if error.kind()==std::io::ErrorKind::NotFound=>Ok(()),Err(error)=>Err(error.into())}}
pub async fn acquire_ownership_safe_lock(path:&Path,retries:LockRetries)->Result<OwnershipSafeLock,LockError>{
    let deadline=tokio::time::Instant::now()+retries.max_timeout*retries.retries;
    loop{
        reject_legacy_directory(path)?;
        let busy=retries.max_timeout.min(deadline.saturating_duration_since(tokio::time::Instant::now())).max(Duration::from_millis(1));
        let acquired=(||{let database=rusqlite::Connection::open(path)?;database.busy_timeout(busy)?;database.execute_batch("BEGIN EXCLUSIVE;")?;Ok::<_,rusqlite::Error>(database)})();
        match acquired{
            Ok(database)=>return Ok(OwnershipSafeLock{database:Some(database)}),
            Err(error)=>{
                reject_legacy_directory(path)?;
                let busy=error.sqlite_error().is_some_and(|error|matches!(error.code,rusqlite::ErrorCode::DatabaseBusy|rusqlite::ErrorCode::DatabaseLocked))||error.to_string().to_lowercase().contains("busy")||error.to_string().to_lowercase().contains("locked");
                if busy&&tokio::time::Instant::now()<deadline{tokio::time::sleep(retries.min_timeout.min(deadline.saturating_duration_since(tokio::time::Instant::now())).max(Duration::from_millis(1))).await;}else{return Err(error.into());}
            }
        }
    }
}
#[cfg(test)]mod tests{
    use super::*;
    fn immediate()->LockRetries{LockRetries{retries:0,min_timeout:Duration::ZERO,max_timeout:Duration::ZERO}}
    #[tokio::test]async fn sqlite_exclusive_lock_blocks_contender_and_release_is_idempotent(){let temp=tempfile::tempdir().unwrap();let path=temp.path().join("daemon.lock");let mut held=acquire_ownership_safe_lock(&path,immediate()).await.unwrap();assert!(matches!(acquire_ownership_safe_lock(&path,immediate()).await,Err(LockError::Sqlite(_))));held.release().unwrap();held.release().unwrap();let mut successor=acquire_ownership_safe_lock(&path,immediate()).await.unwrap();successor.release().unwrap();assert!(path.exists());}
    #[tokio::test]async fn directory_artifact_is_typed_and_never_removed(){let temp=tempfile::tempdir().unwrap();let path=temp.path().join("legacy");std::fs::create_dir(&path).unwrap();assert!(matches!(acquire_ownership_safe_lock(&path,immediate()).await,Err(LockError::LegacyLockArtifact(_))));assert!(path.is_dir());}
}
