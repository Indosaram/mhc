use super::{RecoveryError, derive::random};
use crate::sync::crypto::signatures::sha256_b64;
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
pub fn hold_writes(state: &Path, account: &str, credential: &str) -> Result<(), RecoveryError> {
    let target = hold_path(state, account);
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(state.join("recovery-hold"))?;
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
    cleanup
}
pub fn release_write_hold(state: &Path, account: &str) -> Result<(), RecoveryError> {
    match fs::remove_file(hold_path(state, account)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}
