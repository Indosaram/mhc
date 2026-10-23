use super::{RecoveryError, derive::random};
use crate::{
    journal::{JournalError, sync_journal_directory},
    sync::crypto::signatures::sha256_b64,
};
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

#[derive(Debug, PartialEq, Eq)]
pub enum WriteHold {
    Released,
    Held { credential_id: Option<String> },
}
pub const WRITE_HOLD_NEXT_STEP: &str =
    "Run recovery again on this device; writes resume once its key rotation is stored.";
fn hold_path(state: &Path, account: &str) -> PathBuf {
    state
        .join("recovery-hold")
        .join(format!("{}.json", sha256_b64(account.as_bytes())))
}
pub fn load_write_hold(state: &Path, account: &str) -> Result<WriteHold, RecoveryError> {
    let raw = match fs::read(hold_path(state, account)) {
        Ok(raw) => raw,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(WriteHold::Released),
        Err(error) => return Err(error.into()),
    };
    let credential_id = serde_json::from_slice::<serde_json::Value>(&raw)
        .ok()
        .and_then(|value| {
            if value["account"] == account {
                value["credential_id"].as_str().map(str::to_owned)
            } else {
                None
            }
        });
    Ok(WriteHold::Held { credential_id })
}
fn sync_dir(dir: &Path) -> Result<(), RecoveryError> {
    sync_journal_directory(dir).map_err(|error| match error {
        JournalError::Io(error) => RecoveryError::Io(error),
        other => RecoveryError::Io(io::Error::other(other.to_string())),
    })
}
/// The topmost missing directory on the way to `state`, read before `create` so a retry cannot skip it.
fn topmost_missing(state: &Path) -> Result<Option<PathBuf>, RecoveryError> {
    let mut missing = None;
    let mut dir = std::path::absolute(state)?;
    loop {
        match fs::symlink_metadata(&dir) {
            Ok(_) => return Ok(missing),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        missing = Some(dir.clone());
        match dir.parent() {
            Some(parent) => dir = parent.to_path_buf(),
            None => return Ok(missing),
        }
    }
}
/// Directories to fsync so `state` and its entries are durable: `state`, plus, when `created` was
/// missing, each new entry in its holding directory from the parent of `created` down to `state`.
fn entries_to_sync(state: &Path, created: Option<&Path>) -> Result<Vec<PathBuf>, RecoveryError> {
    let state = std::path::absolute(state)?;
    let mut dirs = vec![state.clone()];
    let Some(top) = created else {
        return Ok(dirs);
    };
    if !state.starts_with(top) {
        return Ok(dirs);
    }
    let mut dir = state.as_path();
    while dir != top {
        let Some(parent) = dir.parent() else { break };
        dirs.insert(0, parent.to_path_buf());
        dir = parent;
    }
    if let Some(parent) = top.parent() {
        dirs.insert(0, parent.to_path_buf());
    }
    Ok(dirs)
}
pub fn hold_writes(state: &Path, account: &str, credential: &str) -> Result<(), RecoveryError> {
    let target = hold_path(state, account);
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    let hold_dir = state.join("recovery-hold");
    let created = topmost_missing(state)?;
    builder.create(&hold_dir)?;
    for entry in entries_to_sync(state, created.as_deref())? {
        sync_dir(&entry)?;
    }
    let suffix: String = random::<6>()?.iter().map(|b| format!("{b:02x}")).collect();
    let staging = target.with_extension(format!("json.{}.{suffix}.tmp", std::process::id()));
    let result = (|| -> Result<(), RecoveryError> {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&staging)?;
        writeln!(
            file,
            "{}",
            serde_json::json!({"version":1,"account":account,"credential_id":credential})
        )?;
        file.sync_all()?;
        fs::rename(&staging, target)?;
        Ok(())
    })();
    let cleanup = match fs::remove_file(&staging) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(RecoveryError::Io(error)),
    };
    result?;
    cleanup?;
    sync_dir(&hold_dir)
}
pub fn release_write_hold(state: &Path, account: &str) -> Result<(), RecoveryError> {
    match fs::remove_file(hold_path(state, account)) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    sync_dir(&state.join("recovery-hold"))
}
