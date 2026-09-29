use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::manifest::{
    CodegraphProvisionAsset, CodegraphProvisionManifest, codegraph_provision_manifest,
};
use crate::runtime::{node_arch, node_platform};

const DEFAULT_LOCK_WAIT_MS: u64 = 5_000;
const DEFAULT_LOCK_STALE_MS: u64 = 120_000;

pub type DownloaderFn<'a> = &'a dyn Fn(&CodegraphProvisionAsset) -> Result<Vec<u8>, String>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodegraphProvisionResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bin_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub provisioned: bool,
}

pub struct EnsureCodegraphProvisionedOptions<'a> {
    pub downloader: Option<DownloaderFn<'a>>,
    pub download_timeout_ms: Option<u64>,
    pub force_bad_checksum: Option<bool>,
    pub install_dir: Option<PathBuf>,
    pub lock_dir: PathBuf,
    pub lock_stale_ms: Option<u64>,
    pub lock_wait_ms: Option<u64>,
    pub manifest: Option<CodegraphProvisionManifest>,
    pub platform_key: Option<String>,
    pub version: String,
    pub sleep_fn: Option<&'a dyn Fn(u64)>,
    pub now_ms_fn: Option<&'a dyn Fn() -> u64>,
}

fn platform_key() -> String {
    format!("{}-{}", node_platform(), node_arch())
}

fn marker_path(install_dir: &Path, version: &str) -> PathBuf {
    install_dir
        .join(".provisioned")
        .join(format!("codegraph-{version}.json"))
}

fn default_install_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from(""))
        .join(".maho")
        .join("codegraph")
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn get_hostname() -> String {
    #[cfg(unix)]
    {
        let mut buf = [0u8; 256];
        let res = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
        if res == 0 {
            let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
            if let Ok(s) = std::str::from_utf8(&buf[..len]) {
                return s.to_string();
            }
        }
    }
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "localhost".to_string())
}

fn random_staging_name() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut hasher = Sha256::new();
    hasher.update(format!("{pid}-{count}-{now}"));
    hex::encode(&hasher.finalize()[..16])
}

fn read_marker(path: &Path, version: &str) -> Option<PathBuf> {
    if !path.exists() {
        return None;
    }
    let raw = fs::read_to_string(path).ok()?;
    let value = serde_json::from_str::<serde_json::Value>(&raw).ok()?;
    let obj = value.as_object()?;
    let marker_ver = obj.get("version")?.as_str()?;
    let bin_path_str = obj.get("binPath")?.as_str()?;
    if marker_ver == version {
        let bin = PathBuf::from(bin_path_str);
        if bin.exists() {
            return Some(bin);
        }
    }
    None
}

struct LockGuard {
    lock_path: PathBuf,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.lock_path);
    }
}

fn default_sleep(ms: u64) {
    std::thread::sleep(std::time::Duration::from_millis(ms));
}

fn default_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn acquire_lock(
    lock_path: &Path,
    wait_ms: u64,
    stale_ms: u64,
    sleep_fn: Option<&dyn Fn(u64)>,
    now_ms_fn: Option<&dyn Fn() -> u64>,
) -> Result<LockGuard, ()> {
    let now = || match now_ms_fn {
        Some(f) => f(),
        None => default_now_ms(),
    };
    let sleep = |ms| match sleep_fn {
        Some(f) => f(ms),
        None => default_sleep(ms),
    };

    let started_at = now();
    if let Some(parent) = lock_path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    while now().saturating_sub(started_at) <= wait_ms {
        match fs::create_dir(lock_path) {
            Ok(()) => {
                return Ok(LockGuard {
                    lock_path: lock_path.to_path_buf(),
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if let Ok(meta) = fs::metadata(lock_path)
                    && let Ok(mtime) = meta.modified()
                {
                    let mtime_ms = mtime
                        .duration_since(SystemTime::UNIX_EPOCH)
                        .map(|d| d.as_millis() as u64)
                        .unwrap_or(0);
                    if now().saturating_sub(mtime_ms) > stale_ms {
                        let _ = fs::remove_dir_all(lock_path);
                        continue;
                    }
                }
                sleep(25);
            }
            Err(_) => {
                sleep(25);
            }
        }
    }

    Err(())
}

fn extract_tar_gz(archive_path: &Path, destination_dir: &Path) -> Result<(), String> {
    let status = Command::new("tar")
        .arg("-xzf")
        .arg(archive_path)
        .arg("-C")
        .arg(destination_dir)
        .status()
        .map_err(|e| format!("failed to execute tar: {e}"))?;
    if !status.success() {
        return Err(format!(
            "tar extraction failed with exit code {:?}",
            status.code()
        ));
    }
    Ok(())
}

fn install_extracted_bundle(
    extract_dir: &Path,
    install_dir: &Path,
    executable_name: &str,
) -> Result<PathBuf, String> {
    let entries = fs::read_dir(extract_dir)
        .map_err(|e| format!("failed to read extract dir: {e}"))?
        .flatten()
        .filter(|e| e.path().is_dir())
        .collect::<Vec<_>>();

    if entries.len() != 1 {
        return Err(format!(
            "CodeGraph archive should contain one root directory, found {}",
            entries.len()
        ));
    }

    let bundle_dir = entries[0].path();
    let bundle_entries = fs::read_dir(&bundle_dir)
        .map_err(|e| format!("failed to read bundle dir: {e}"))?
        .flatten()
        .collect::<Vec<_>>();

    fs::create_dir_all(install_dir).map_err(|e| format!("failed to create install dir: {e}"))?;

    for entry in bundle_entries {
        let name = entry.file_name();
        let dest = install_dir.join(&name);
        if dest.is_dir() {
            let _ = fs::remove_dir_all(&dest);
        } else if dest.exists() {
            let _ = fs::remove_file(&dest);
        }
        fs::rename(entry.path(), &dest)
            .map_err(|e| format!("failed to move bundle entry {}: {e}", dest.display()))?;
    }

    let destination = install_dir.join("bin").join(executable_name);
    if !destination.exists() {
        return Err(format!(
            "CodeGraph archive did not contain bin/{executable_name}"
        ));
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&destination, fs::Permissions::from_mode(0o755));
    }

    Ok(destination)
}

struct ForcedBadChecksum {
    install_dir: PathBuf,
    manifest: CodegraphProvisionManifest,
    platform_key: String,
}

fn forced_bad_checksum_options(
    options: &EnsureCodegraphProvisionedOptions<'_>,
) -> Option<ForcedBadChecksum> {
    if options.force_bad_checksum != Some(true) {
        return None;
    }
    let key = options.platform_key.clone().unwrap_or_else(platform_key);
    let exec_name = if node_platform() == "win32" {
        "codegraph.cmd"
    } else {
        "codegraph"
    };

    let mut assets = std::collections::BTreeMap::new();
    assets.insert(
        key.clone(),
        CodegraphProvisionAsset {
            executable_name: exec_name.to_string(),
            sha256: "0000".to_string(),
            url: "memory://bad".to_string(),
        },
    );

    let install_dir = options
        .install_dir
        .clone()
        .unwrap_or_else(|| options.lock_dir.join("codegraph-force-bad-checksum"));

    Some(ForcedBadChecksum {
        install_dir,
        manifest: CodegraphProvisionManifest {
            assets,
            version: options.version.clone(),
        },
        platform_key: key,
    })
}

fn install_asset(
    asset: &CodegraphProvisionAsset,
    downloader: &dyn Fn(&CodegraphProvisionAsset) -> Result<Vec<u8>, String>,
    install_dir: &Path,
    version: &str,
) -> Result<PathBuf, String> {
    let staging_dir = install_dir.join(".staging").join(random_staging_name());
    let url_filename = asset.url.rsplit('/').next().unwrap_or("codegraph.tar.gz");
    let archive_path = staging_dir.join(url_filename);
    let extract_dir = staging_dir.join("extract");

    let result = (|| -> Result<PathBuf, String> {
        fs::create_dir_all(&extract_dir)
            .map_err(|e| format!("failed to create extract directory: {e}"))?;

        let bytes = downloader(asset)?;
        let actual_checksum = sha256_hex(&bytes);
        if actual_checksum != asset.sha256 {
            return Err(format!(
                "checksum mismatch for {url_filename}: expected {}, got {actual_checksum}",
                asset.sha256
            ));
        }

        if !asset.url.ends_with(".tar.gz") && !asset.url.ends_with(".tgz") {
            return Err(format!(
                "unsupported CodeGraph archive type for {url_filename}"
            ));
        }

        fs::write(&archive_path, &bytes).map_err(|e| format!("failed to write archive: {e}"))?;
        extract_tar_gz(&archive_path, &extract_dir)?;
        let destination =
            install_extracted_bundle(&extract_dir, install_dir, &asset.executable_name)?;

        let prov_dir = install_dir.join(".provisioned");
        fs::create_dir_all(&prov_dir)
            .map_err(|e| format!("failed to create .provisioned dir: {e}"))?;

        let marker_file = marker_path(install_dir, version);
        let marker_content = serde_json::to_string_pretty(&serde_json::json!({
            "binPath": destination.to_string_lossy(),
            "version": version
        }))
        .map_err(|e| format!("failed to serialize marker: {e}"))?;

        fs::write(&marker_file, format!("{marker_content}\n"))
            .map_err(|e| format!("failed to write marker: {e}"))?;

        Ok(destination)
    })();

    let _ = fs::remove_dir_all(&staging_dir);
    let staging_parent = install_dir.join(".staging");
    let _ = fs::remove_dir(&staging_parent);

    result
}

pub fn ensure_codegraph_provisioned(
    options: &EnsureCodegraphProvisionedOptions<'_>,
) -> CodegraphProvisionResult {
    let forced = forced_bad_checksum_options(options);
    let install_dir = forced
        .as_ref()
        .map(|f| f.install_dir.clone())
        .or_else(|| options.install_dir.clone())
        .unwrap_or_else(default_install_dir);
    let manifest = forced
        .as_ref()
        .map(|f| f.manifest.clone())
        .or_else(|| options.manifest.clone())
        .unwrap_or_else(codegraph_provision_manifest);
    let active_platform_key = forced
        .as_ref()
        .map(|f| f.platform_key.clone())
        .or_else(|| options.platform_key.clone())
        .unwrap_or_else(platform_key);

    let marker = marker_path(&install_dir, &options.version);
    if let Some(existing) = read_marker(&marker, &options.version) {
        return CodegraphProvisionResult {
            bin_path: Some(existing),
            error: None,
            provisioned: true,
        };
    }

    let lock_path = options
        .lock_dir
        .join(format!("codegraph-{}.lock", get_hostname()));
    let Ok(_lock_guard) = acquire_lock(
        &lock_path,
        options.lock_wait_ms.unwrap_or(DEFAULT_LOCK_WAIT_MS),
        options.lock_stale_ms.unwrap_or(DEFAULT_LOCK_STALE_MS),
        options.sleep_fn,
        options.now_ms_fn,
    ) else {
        return CodegraphProvisionResult {
            bin_path: None,
            error: Some("timed out waiting for codegraph provisioning lock".to_string()),
            provisioned: false,
        };
    };

    if let Some(existing) = read_marker(&marker, &options.version) {
        return CodegraphProvisionResult {
            bin_path: Some(existing),
            error: None,
            provisioned: true,
        };
    }

    if manifest.version != options.version {
        return CodegraphProvisionResult {
            bin_path: None,
            error: Some(format!(
                "manifest version {} does not match requested {}",
                manifest.version, options.version
            )),
            provisioned: false,
        };
    }

    let Some(asset) = manifest.assets.get(&active_platform_key) else {
        return CodegraphProvisionResult {
            bin_path: None,
            error: Some(format!(
                "no CodeGraph {} asset for {active_platform_key}",
                options.version
            )),
            provisioned: false,
        };
    };

    let bad_checksum_downloader = |_asset: &CodegraphProvisionAsset| -> Result<Vec<u8>, String> {
        Ok(b"checksum mismatch".to_vec())
    };

    let downloader_fn: &dyn Fn(&CodegraphProvisionAsset) -> Result<Vec<u8>, String> =
        if forced.is_some() {
            &bad_checksum_downloader
        } else if let Some(d) = options.downloader {
            d
        } else {
            return CodegraphProvisionResult {
                bin_path: None,
                error: Some("no downloader provided".to_string()),
                provisioned: false,
            };
        };

    match install_asset(asset, downloader_fn, &install_dir, &options.version) {
        Ok(destination) => CodegraphProvisionResult {
            bin_path: Some(destination),
            error: None,
            provisioned: true,
        },
        Err(err) => CodegraphProvisionResult {
            bin_path: None,
            error: Some(err),
            provisioned: false,
        },
    }
}
