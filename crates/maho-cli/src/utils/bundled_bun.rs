//! Port of `packages/coding-agent/src/utils/bundled-bun.ts` (omo#9362, omo#10762).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, PoisonError};

use sha2::{Digest, Sha256};

use crate::engine_trust::adopted::adopted_engine_trust;
use crate::engine_trust::marker::EngineTrustMarker;

use super::bundled_bun_shim::{cmd_shim_files, posix_shim_files};

#[derive(Clone, Debug)]
pub struct BundledBunHost {
    pub main: Option<String>,
    pub exec_path: String,
    pub platform: String,
    pub shim_roots: Vec<PathBuf>,
    pub trust: Option<EngineTrustMarker>,
}

pub fn is_compiled_bun_executable(entry: Option<&str>) -> bool {
    let Some(entry) = entry else {
        return false;
    };
    if entry.starts_with("/$bunfs/") {
        return true;
    }
    let bytes = entry.as_bytes();
    bytes.len() >= 7
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && entry[2..].starts_with("\\~BUN\\")
}

fn current_host() -> BundledBunHost {
    let platform = if cfg!(windows) {
        "win32"
    } else {
        std::env::consts::OS
    };
    let agent_dir = maho_core::config::get_agent_dir();
    BundledBunHost {
        main: None,
        exec_path: std::env::current_exe()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default(),
        platform: platform.to_owned(),
        shim_roots: vec![
            PathBuf::from(agent_dir).join("bundled-bun"),
            std::env::temp_dir().join("senpi-bundled-bun"),
        ],
        trust: adopted_engine_trust(),
    }
}

fn shim_key(host: &BundledBunHost) -> String {
    let Some(trust) = &host.trust else {
        return host.exec_path.clone();
    };
    let closure = match &trust.defaults {
        None => "opt-in".to_owned(),
        Some(defaults) => format!("{}\0{}", defaults.file, defaults.dir),
    };
    let fallback = trust.fallback_reason.map_or("complete", |r| r.as_str());
    [
        host.exec_path.as_str(),
        trust.file.as_str(),
        closure.as_str(),
        match trust.owner {
            crate::engine_trust::EngineTrustOwner::Server => "server",
            crate::engine_trust::EngineTrustOwner::Standalone => "standalone",
        },
        fallback,
    ]
    .join("\0")
}

fn digest_prefix(input: &str) -> String {
    let hash = Sha256::digest(input.as_bytes());
    let mut hex = String::with_capacity(16);
    for byte in &hash[..8] {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

fn write_if_changed(path: &Path, content: &str, executable: bool) -> std::io::Result<()> {
    if let Ok(existing) = std::fs::read_to_string(path)
        && existing == content
    {
        return Ok(());
    }
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&tmp, content)?;
    #[cfg(unix)]
    if executable {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    }
    #[cfg(not(unix))]
    let _ = executable;
    std::fs::rename(&tmp, path)
}

fn materialize(root: &Path, host: &BundledBunHost) -> Option<PathBuf> {
    let dir = root.join(digest_prefix(&shim_key(host)));
    std::fs::create_dir_all(&dir).ok()?;
    let files = if host.platform == "win32" {
        cmd_shim_files(&host.exec_path, &dir.to_string_lossy(), host.trust.as_ref())
    } else {
        posix_shim_files(&host.exec_path, &dir.to_string_lossy(), host.trust.as_ref())
    };
    let executable = host.platform != "win32";
    for (name, content) in files {
        write_if_changed(&dir.join(name), &content, executable).ok()?;
    }
    Some(dir)
}

static RESOLVED: LazyLock<Mutex<BTreeMap<String, Option<PathBuf>>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));

pub fn bundled_bun_dir(host: &BundledBunHost) -> Option<PathBuf> {
    if !is_compiled_bun_executable(host.main.as_deref()) {
        return None;
    }
    let roots: Vec<String> = host
        .shim_roots
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    let key = format!("{}\0{}", shim_key(host), roots.join("\0"));
    if let Some(cached) = RESOLVED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&key)
        .cloned()
    {
        return cached;
    }
    let dir = host
        .shim_roots
        .iter()
        .find_map(|root| materialize(root, host));
    RESOLVED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(key, dir.clone());
    dir
}

pub fn with_bundled_bun_commands(mut env: BTreeMap<String, String>) -> BTreeMap<String, String> {
    let host = current_host();
    let Some(dir) = bundled_bun_dir(&host) else {
        return env;
    };
    let dir_str = dir.to_string_lossy().into_owned();
    let delimiter = if cfg!(windows) { ';' } else { ':' };
    let path_key = env
        .keys()
        .find(|k| k.eq_ignore_ascii_case("path"))
        .cloned()
        .unwrap_or_else(|| "PATH".to_owned());
    let current = env.get(&path_key).cloned().unwrap_or_default();
    if current.split(delimiter).any(|entry| entry == dir_str) {
        return env;
    }
    env.insert(
        path_key,
        if current.is_empty() {
            dir_str
        } else {
            format!("{current}{delimiter}{dir_str}")
        },
    );
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn materializes_shims_under_compiled_bun_root() {
        let dir = tempfile::tempdir().expect("tempdir");
        let host = BundledBunHost {
            main: Some("/$bunfs/root/app".into()),
            exec_path: "/opt/senpi/bin/senpi".into(),
            platform: "linux".into(),
            shim_roots: vec![dir.path().to_path_buf()],
            trust: None,
        };
        let out = bundled_bun_dir(&host).expect("shim dir");
        assert!(out.join("bun").exists());
        assert!(out.join("bunx").exists());
    }
}
