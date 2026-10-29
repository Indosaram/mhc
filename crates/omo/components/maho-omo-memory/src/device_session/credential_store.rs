//! Native Linux Secret Service storage; no disk fallback and no terminal notices.
use super::account_source::{AccountFuture, CredentialStore};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::sync::Arc;
pub struct OsCredentialStore {
    name: String,
}
impl OsCredentialStore {
    pub fn for_origin(origin: &str) -> Arc<dyn CredentialStore> {
        Arc::new(Self {
            name: format!("{:x}", Sha256::digest(origin.as_bytes())),
        })
    }
}
impl CredentialStore for OsCredentialStore {
    fn read(&self) -> AccountFuture<'_, Option<Value>> {
        Box::pin(async move {
            #[cfg(target_os = "linux")]
            {
                let mut probe = tokio::process::Command::new("gdbus");
                probe.args(["call", "--session", "--dest", "org.freedesktop.secrets", "--object-path", "/org/freedesktop/secrets/aliases/default", "--method", "org.freedesktop.DBus.Properties.Get", "org.freedesktop.Secret.Collection", "Locked"]).stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null()).kill_on_drop(true);
                if let Ok(Ok(reply)) = tokio::time::timeout(std::time::Duration::from_secs(10), probe.output()).await
                    && (!reply.status.success() || String::from_utf8_lossy(&reply.stdout).contains("<true>")) { return Ok(None); }
                let output = match tokio::process::Command::new("secret-tool")
                    .args(["lookup", "service", "omo-cli", "name", &self.name])
                    .stdin(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .output()
                    .await
                {
                    Ok(output) => output,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                    Err(error) => return Err(error.to_string()),
                };
                if !output.status.success() || output.stdout.is_empty() {
                    return Ok(None);
                }
                serde_json::from_slice(&output.stdout)
                    .map(Some)
                    .map_err(|error| error.to_string())
            }
            #[cfg(not(target_os = "linux"))]
            {
                Ok(None)
            }
        })
    }
    fn write(&self, value: Value) -> AccountFuture<'_, ()> {
        Box::pin(async move {
            use tokio::io::AsyncWriteExt;
            let mut child = tokio::process::Command::new("secret-tool")
                .args([
                    "store",
                    "--label=OmO sign-in",
                    "service",
                    "omo-cli",
                    "name",
                    &self.name,
                ])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .map_err(|error| error.to_string())?;
            let mut stdin = child
                .stdin
                .take()
                .ok_or("credential store stdin unavailable")?;
            stdin
                .write_all(value.to_string().as_bytes())
                .await
                .map_err(|error| error.to_string())?;
            drop(stdin);
            if !child
                .wait()
                .await
                .map_err(|error| error.to_string())?
                .success()
            {
                return Err("OS credential store unavailable".into());
            }
            Ok(())
        })
    }
    fn clear(&self) -> AccountFuture<'_, ()> {
        Box::pin(async move {
            let status = tokio::process::Command::new("secret-tool")
                .args(["clear", "service", "omo-cli", "name", &self.name])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .await
                .map_err(|error| error.to_string())?;
            if status.success() {
                Ok(())
            } else {
                Err("OS credential store clear failed".into())
            }
        })
    }
}
