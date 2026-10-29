use std::collections::HashMap;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::{LazyLock, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use super::candidates::plan_sg_candidates;
use super::{
    SG_BINARY_NOT_FOUND, SgBinaryNotFoundError, SgCandidate, SgResolution, SgResolutionTier,
    sg_binary_not_found_message, sg_install_hints,
};
use crate::runtime::{bun_which, node_arch, node_platform};

pub const SG_VERSION_PROBE_TIMEOUT_MS: u64 = 5_000;

pub type SgFileExists<'a> = &'a dyn Fn(&str) -> bool;
pub type SgVersionProbe<'a> = &'a dyn Fn(&str) -> Result<String, String>;
pub type SgWhich<'a> = &'a dyn Fn(&str) -> Option<String>;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SgCacheMode {
    #[default]
    Use,
    Bypass,
}

/// Injectable inputs for [`resolve_sg_binary_sync`]; every `None` falls back to the real host.
#[derive(Default)]
pub struct SgResolverOptions<'a> {
    pub arch: Option<&'a str>,
    pub cache: SgCacheMode,
    /// Environment to read; `None` reads the process environment.
    pub env: Option<&'a HashMap<String, String>>,
    pub file_exists: Option<SgFileExists<'a>>,
    pub home_dir: Option<&'a str>,
    pub package_dir: Option<&'a str>,
    pub platform: Option<&'a str>,
    /// Re-probe a cached binary before returning it.
    pub revalidate: bool,
    pub runtime_dir: Option<&'a str>,
    pub run_version_probe: Option<SgVersionProbe<'a>>,
    pub which: Option<SgWhich<'a>>,
}

struct CacheEntry {
    fingerprint: String,
    path: String,
    tier: SgResolutionTier,
}

static CACHE: LazyLock<Mutex<Option<CacheEntry>>> = LazyLock::new(|| Mutex::new(None));

pub fn clear_sg_resolution_cache() {
    *CACHE.lock().unwrap_or_else(PoisonError::into_inner) = None;
}

fn default_file_exists(path: &str) -> bool {
    std::fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.len() > 0)
}

/// Run `<binary> --version` with stderr discarded and a hard 5s deadline.
fn default_version_probe(binary: &str) -> Result<String, String> {
    let mut child = Command::new(binary)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| error.to_string())?;
    let mut stdout = child.stdout.take().ok_or("stdout unavailable")?;
    let reader = thread::spawn(move || {
        let mut text = String::new();
        let _ = stdout.read_to_string(&mut text);
        text
    });
    let deadline = Instant::now() + Duration::from_millis(SG_VERSION_PROBE_TIMEOUT_MS);
    loop {
        match child.try_wait().map_err(|error| error.to_string())? {
            Some(status) if status.success() => {
                return reader
                    .join()
                    .map_err(|_| "probe reader panicked".to_string());
            }
            Some(status) => return Err(format!("probe exited with {status}")),
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("probe timed out".to_string());
            }
            None => thread::sleep(Duration::from_millis(20)),
        }
    }
}

struct Deps<'a> {
    file_exists: &'a dyn Fn(&str) -> bool,
    probe: &'a dyn Fn(&str) -> Result<String, String>,
    which: &'a dyn Fn(&str) -> Option<String>,
}

impl Deps<'_> {
    fn probe_passes(&self, path: &str) -> bool {
        (self.probe)(path).is_ok_and(|output| output.to_lowercase().contains("ast-grep"))
    }

    fn accepts(&self, path: &str) -> bool {
        (self.file_exists)(path) && self.probe_passes(path)
    }

    fn first_accepted(&self, candidates: &[SgCandidate]) -> Option<SgCandidate> {
        candidates
            .iter()
            .find(|candidate| self.accepts(&candidate.path))
            .cloned()
    }
}

fn not_found(platform: &str) -> SgResolution {
    SgResolution::NotFound {
        error: SgBinaryNotFoundError {
            code: SG_BINARY_NOT_FOUND,
            hints: sg_install_hints(platform),
            message: sg_binary_not_found_message(platform),
        },
    }
}

fn default_which(name: &str) -> Option<String> {
    bun_which(name).map(|path| path.to_string_lossy().into_owned())
}

/// Resolve `sg` across env override, OMO runtime, skill bin, PATH, then Homebrew. Every candidate
/// must be a non-empty file whose `--version` output mentions `ast-grep`; failures fall through.
pub fn resolve_sg_binary_sync(options: &SgResolverOptions<'_>) -> SgResolution {
    let platform = options.platform.unwrap_or(node_platform());
    let deps = Deps {
        file_exists: options.file_exists.unwrap_or(&default_file_exists),
        probe: options.run_version_probe.unwrap_or(&default_version_probe),
        which: options.which.unwrap_or(&default_which),
    };
    let plan = plan_sg_candidates(options);
    let fingerprint = serde_json::json!([
        platform,
        options.arch.unwrap_or(node_arch()),
        plan.before_path
            .iter()
            .map(|candidate| candidate.path.as_str())
            .collect::<Vec<_>>(),
    ])
    .to_string();
    let use_cache = options.cache == SgCacheMode::Use;
    if use_cache {
        let mut cache = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(entry) = cache
            .as_ref()
            .filter(|entry| entry.fingerprint == fingerprint)
        {
            let valid = (deps.file_exists)(&entry.path)
                && (!options.revalidate || deps.probe_passes(&entry.path));
            if valid {
                return SgResolution::Found {
                    path: entry.path.clone(),
                    tier: entry.tier,
                };
            }
            *cache = None;
        }
    }
    let path_candidates: Vec<SgCandidate> = plan
        .path_commands
        .iter()
        .filter_map(|name| (deps.which)(name))
        .map(|path| SgCandidate {
            path,
            tier: SgResolutionTier::Path,
        })
        .collect();
    let accepted = deps
        .first_accepted(&plan.before_path)
        .or_else(|| deps.first_accepted(&path_candidates))
        .or_else(|| deps.first_accepted(&plan.after_path));
    match accepted {
        Some(found) => {
            if use_cache {
                *CACHE.lock().unwrap_or_else(PoisonError::into_inner) = Some(CacheEntry {
                    fingerprint,
                    path: found.path.clone(),
                    tier: found.tier,
                });
            }
            SgResolution::Found {
                path: found.path,
                tier: found.tier,
            }
        }
        None => not_found(platform),
    }
}

pub fn find_sg_binary_sync(options: &SgResolverOptions<'_>) -> Option<String> {
    match resolve_sg_binary_sync(options) {
        SgResolution::Found { path, .. } => Some(path),
        SgResolution::NotFound { .. } => None,
    }
}
