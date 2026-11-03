//! Port of senpi `auth/legacy-migration.ts`: claim a legacy token record into the per-server
//! store under the legacy migration lock (sync and async forms).
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

pub type ReadLegacyRecord<R> = Box<dyn Fn(&std::path::Path) -> Option<R> + Send + Sync>;
pub type WriteLegacyRecord<R> = Box<dyn Fn(&R) -> std::io::Result<()> + Send + Sync>;

pub struct LegacyMigration<R> {
    pub root_dir: PathBuf,
    pub lock_path: PathBuf,
    pub legacy_tokens: PathBuf,
    pub tokens_path: PathBuf,
    pub stale_ms: u64,
    pub disable_lock: bool,
    pub read: ReadLegacyRecord<R>,
    pub write: WriteLegacyRecord<R>,
    pub lock_error: Box<dyn Fn(std::io::Error) -> std::io::Error + Send + Sync>,
}

fn rm_force(path: &std::path::Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

fn mkdir_private(path: &std::path::Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

/// `adoptLegacyRecord`: synchronous claim under the blocking lock.
pub fn adopt_legacy_record<R>(options: &LegacyMigration<R>) -> std::io::Result<Option<R>> {
    // Nothing to migrate needs no lock: every token lookup lands here when a server has no saved grant.
    if !options.legacy_tokens.exists() {
        return Ok(None);
    }
    let release = acquire_sync(options)?;
    let result = claim(options);
    release();
    result
}

/// `readRecordAsync`: the current record, or the claimed legacy one under the async lock.
pub async fn read_record_async<R>(options: &LegacyMigration<R>) -> std::io::Result<Option<R>> {
    if let Some(record) = (options.read)(&options.tokens_path) {
        return Ok(Some(record));
    }
    if !options.legacy_tokens.exists() {
        return Ok(None);
    }
    mkdir_private(&options.root_dir)?;
    let guard = if options.disable_lock {
        None
    } else {
        Some(
            super::legacy_lock::acquire_legacy_lock(&options.lock_path, options.stale_ms)
                .await
                .map_err(|cause| (options.lock_error)(cause))?,
        )
    };
    let result = claim(options);
    if let Some(guard) = guard {
        guard.release().await?;
    }
    result
}

fn claim<R>(options: &LegacyMigration<R>) -> std::io::Result<Option<R>> {
    let Some(legacy) = (options.read)(&options.legacy_tokens) else {
        return Ok(None);
    };
    if let Some(existing) = (options.read)(&options.tokens_path) {
        rm_force(&options.legacy_tokens)?;
        return Ok(Some(existing));
    }
    (options.write)(&legacy)?;
    rm_force(&options.legacy_tokens)?;
    Ok(Some(legacy))
}

fn acquire_sync<R>(options: &LegacyMigration<R>) -> std::io::Result<Box<dyn FnOnce()>> {
    if options.disable_lock {
        return Ok(Box::new(|| {}));
    }
    mkdir_private(&options.root_dir)?;
    let deadline = Instant::now() + Duration::from_millis(options.stale_ms);
    loop {
        let mut open = std::fs::OpenOptions::new();
        open.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            open.mode(0o600);
        }
        match open.open(&options.lock_path) {
            Ok(file) => {
                let path = options.lock_path.clone();
                return Ok(Box::new(move || {
                    drop(file);
                    let _ = rm_force(&path);
                }));
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        let age = match std::fs::metadata(&options.lock_path) {
            Ok(metadata) => SystemTime::now()
                .duration_since(metadata.modified()?)
                .map_or(0, |age| age.as_millis()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if age >= u128::from(options.stale_ms) {
            rm_force(&options.lock_path)?;
            continue;
        }
        if Instant::now() >= deadline {
            return Err((options.lock_error)(std::io::Error::other(format!(
                "legacy migration lock held for {age}ms"
            ))));
        }
        // Preserve the synchronous API; background preparation uses read_record_async.
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn migration(dir: &std::path::Path, disable_lock: bool) -> LegacyMigration<String> {
        LegacyMigration {
            root_dir: dir.to_path_buf(),
            lock_path: dir.join("legacy.lock"),
            legacy_tokens: dir.join("legacy.json"),
            tokens_path: dir.join("server/tokens.json"),
            stale_ms: 200,
            disable_lock,
            read: Box::new(|path: &std::path::Path| std::fs::read_to_string(path).ok()),
            write: Box::new({
                let target = dir.join("server/tokens.json");
                move |record: &String| {
                    std::fs::create_dir_all(target.parent().expect("parent"))?;
                    std::fs::write(&target, record)
                }
            }),
            lock_error: Box::new(|cause: std::io::Error| {
                std::io::Error::other(format!("lock: {cause}"))
            }),
        }
    }

    #[test]
    fn adopts_legacy_record_once_and_removes_it() -> std::io::Result<()> {
        let dir = tempfile::tempdir()?;
        let options = migration(dir.path(), false);
        std::fs::write(&options.legacy_tokens, "legacy")?;
        assert_eq!(adopt_legacy_record(&options)?, Some("legacy".to_owned()));
        assert!(!options.legacy_tokens.exists());
        assert_eq!(std::fs::read_to_string(&options.tokens_path)?, "legacy");
        assert!(!options.lock_path.exists());
        assert_eq!(adopt_legacy_record(&options)?, None);
        Ok(())
    }

    #[test]
    fn existing_record_wins_over_legacy() -> std::io::Result<()> {
        let dir = tempfile::tempdir()?;
        let options = migration(dir.path(), true);
        std::fs::create_dir_all(options.tokens_path.parent().expect("parent"))?;
        std::fs::write(&options.tokens_path, "current")?;
        std::fs::write(&options.legacy_tokens, "legacy")?;
        assert_eq!(adopt_legacy_record(&options)?, Some("current".to_owned()));
        assert!(!options.legacy_tokens.exists());
        Ok(())
    }

    #[tokio::test]
    async fn async_read_prefers_current_and_claims_legacy() -> std::io::Result<()> {
        let dir = tempfile::tempdir()?;
        let options = migration(dir.path(), false);
        assert_eq!(read_record_async(&options).await?, None);
        std::fs::write(&options.legacy_tokens, "legacy")?;
        assert_eq!(
            read_record_async(&options).await?,
            Some("legacy".to_owned())
        );
        assert_eq!(
            read_record_async(&options).await?,
            Some("legacy".to_owned())
        );
        assert!(!options.lock_path.exists());
        Ok(())
    }
}
