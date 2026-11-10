//! Pinned npm native executable acquisition, matching upstream `claude-code/acquire.ts`.
use std::io::Write;
use std::path::{Path, PathBuf};

use base64::Engine;
use serde::Deserialize;
use sha2::{Digest, Sha512};

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeCodePin {
    pub name: String,
    pub version: String,
    pub integrity: String,
    pub claude_code_version: Option<String>,
}

pub fn executable_name() -> &'static str {
    if cfg!(windows) {
        "claude.exe"
    } else {
        "claude"
    }
}

pub fn cached_executable_path(root: &Path, pin: &ClaudeCodePin) -> PathBuf {
    root.join(pin.name.replace('/', "+"))
        .join(&pin.version)
        .join(executable_name())
}

pub fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() || metadata.len() == 0 {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

pub async fn acquire_claude_code(root: &Path, pin: &ClaudeCodePin) -> Result<PathBuf, String> {
    // The pin is an installation boundary: do not let it escape the cache or registry URL.
    if !pin.name.starts_with('@')
        || pin.name.matches('/').count() != 1
        || !pin
            .name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"@/._-".contains(&b))
        || pin.version.is_empty()
        || pin.version == "."
        || pin.version == ".."
        || !pin
            .version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-+".contains(&b))
    {
        return Err("Invalid Claude Code package pin".into());
    }
    let target = cached_executable_path(root, pin);
    if is_executable(&target) {
        return Ok(target);
    }
    let bare = pin
        .name
        .split('/')
        .next_back()
        .ok_or("Invalid package name")?;
    let url = format!(
        "https://registry.npmjs.org/{}/-/{bare}-{}.tgz",
        pin.name, pin.version
    );
    let download = async {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()?
            .get(url)
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await
    }
    .await;
    let bytes = download.map_err(|error| format!(
        "Claude Code {} could not be downloaded ({error}). Connect once, install claude on PATH, or set CLAUDE_CODE_EXECUTABLE.",
        pin.claude_code_version.as_deref().unwrap_or(&pin.version)
    ))?;
    let actual = base64::engine::general_purpose::STANDARD.encode(Sha512::digest(&bytes));
    if pin.integrity.strip_prefix("sha512-") != Some(actual.as_str()) {
        return Err("Claude Code package failed its integrity check; nothing was installed".into());
    }
    let install = || -> Result<PathBuf, Box<dyn std::error::Error>> {
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes.as_ref()));
        let entry_name = PathBuf::from("package").join(executable_name());
        for entry in archive.entries()? {
            let mut entry = entry?;
            if entry.path()?.as_ref() != entry_name || !entry.header().entry_type().is_file() {
                continue;
            }
            let directory = target.parent().ok_or("cache executable has no parent")?;
            std::fs::create_dir_all(directory)?;
            let mut staged = tempfile::NamedTempFile::new_in(directory)?;
            std::io::copy(&mut entry, &mut staged)?;
            staged.flush()?;
            if staged.as_file().metadata()?.len() == 0 {
                return Err("empty Claude Code executable".into());
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                staged
                    .as_file()
                    .set_permissions(std::fs::Permissions::from_mode(0o755))?;
            }
            staged.persist(&target)?;
            return Ok(target.clone());
        }
        Err(format!("Claude Code package has no {}", executable_name()).into())
    };
    install().map_err(|error| error.to_string())
}
