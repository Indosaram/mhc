//! ast-grep (`sg`) binary manifest, candidate planning, resolution, provisioning and skill install.

mod candidates;
mod install_script;
mod provisioner;
mod resolver;

use std::collections::HashMap;

pub use candidates::{SgCandidatePlan, plan_sg_candidates};
pub use install_script::{
    AST_GREP_BIN_DIR_ENV_KEY, AstGrepInstallSpawn, AstGrepInstallSpawnOptions,
    AstGrepInstallSpawnOutcome, AstGrepInstallSpawnedProcess, AstGrepSkillInstallOptions,
    AstGrepSkillInstallResult, KILL_GRACE_MS, ast_grep_runtime_dir, run_ast_grep_skill_install,
};
pub use provisioner::{
    SgFetch, SgFetchResponse, SgProvisionError, SgProvisionErrorCode, SgProvisionOptions,
    provision_sg_binary,
};
pub use resolver::{
    SG_VERSION_PROBE_TIMEOUT_MS, SgCacheMode, SgFileExists, SgResolverOptions, SgVersionProbe,
    SgWhich, clear_sg_resolution_cache, find_sg_binary_sync, resolve_sg_binary_sync,
};

pub const SG_PATH_ENV_KEY: &str = "OMO_AST_GREP_SG_PATH";
pub const SG_BINARY_NOT_FOUND: &str = "BINARY_NOT_FOUND";
pub const SG_PINNED_VERSION: &str = "0.43.0";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SgManifestAsset {
    pub sha256: &'static str,
    pub url: &'static str,
}

/// Pinned release assets keyed by runtime slug.
pub const SG_RELEASE_ASSETS: [(&str, SgManifestAsset); 6] = [
    (
        "darwin-arm64",
        SgManifestAsset {
            sha256: "8c847d0a29aa4b3101b3361e0b3ee7fb53c7e497adc9ed1afc9615538cd40782",
            url: "https://github.com/ast-grep/ast-grep/releases/download/0.43.0/app-aarch64-apple-darwin.zip",
        },
    ),
    (
        "darwin-x64",
        SgManifestAsset {
            sha256: "6d703090b106747b2f56086b6ccc7e798fe78bcae70257aa20519b220153555b",
            url: "https://github.com/ast-grep/ast-grep/releases/download/0.43.0/app-x86_64-apple-darwin.zip",
        },
    ),
    (
        "linux-arm64",
        SgManifestAsset {
            sha256: "e706846148493967f3ab8011334817edd86ce5acbec10718b2a7b40799c640ff",
            url: "https://github.com/ast-grep/ast-grep/releases/download/0.43.0/app-aarch64-unknown-linux-gnu.zip",
        },
    ),
    (
        "linux-x64",
        SgManifestAsset {
            sha256: "a26253a9c821d935f7e383e40f0de7c2ca62a4121de1f73a6d81ec32eae631e0",
            url: "https://github.com/ast-grep/ast-grep/releases/download/0.43.0/app-x86_64-unknown-linux-gnu.zip",
        },
    ),
    (
        "win32-arm64",
        SgManifestAsset {
            sha256: "a519fdd90324bf6858fde2d3feb2b862d67b834dc11af8f5b6c2c8143ab6a6c5",
            url: "https://github.com/ast-grep/ast-grep/releases/download/0.43.0/app-aarch64-pc-windows-msvc.zip",
        },
    ),
    (
        "win32-x64",
        SgManifestAsset {
            sha256: "a4febbc8c48671e5729d85e29e4ebe5a051b7250d19545bca18e725ccf40ef61",
            url: "https://github.com/ast-grep/ast-grep/releases/download/0.43.0/app-x86_64-pc-windows-msvc.zip",
        },
    ),
];

pub fn sg_release_asset(slug: &str) -> Option<SgManifestAsset> {
    SG_RELEASE_ASSETS
        .iter()
        .find(|(key, _)| *key == slug)
        .map(|(_, asset)| *asset)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SgResolutionTier {
    EnvOverride,
    OmoRuntime,
    SkillBin,
    Path,
    Homebrew,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SgCandidate {
    pub path: String,
    pub tier: SgResolutionTier,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SgBinaryNotFoundError {
    pub code: &'static str,
    pub hints: Vec<String>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SgResolution {
    Found {
        path: String,
        tier: SgResolutionTier,
    },
    NotFound {
        error: SgBinaryNotFoundError,
    },
}

pub fn normalize_runtime_platform(platform: &str) -> &'static str {
    match platform {
        "darwin" => "darwin",
        "win32" => "win32",
        _ => "linux",
    }
}

pub fn normalize_runtime_arch(arch: &str) -> &'static str {
    match arch {
        "arm64" | "aarch64" => "arm64",
        _ => "x64",
    }
}

pub fn runtime_slug(platform: &str, arch: &str) -> String {
    format!(
        "{}-{}",
        normalize_runtime_platform(platform),
        normalize_runtime_arch(arch)
    )
}

pub fn sg_binary_name(platform: &str) -> &'static str {
    if normalize_runtime_platform(platform) == "win32" {
        "sg.exe"
    } else {
        "sg"
    }
}

const OMO_PROVISION_HINT: &str = "Start an OMO session so the bundled ast-grep skill provisions the pinned runtime automatically";

pub fn sg_install_hints(platform: &str) -> Vec<String> {
    let platform_hints: &[&str] = match platform {
        "darwin" => &[
            "brew install ast-grep",
            "npm install -g @ast-grep/cli",
            "cargo install ast-grep --locked",
        ],
        "win32" => &[
            "scoop install main/ast-grep",
            "winget install ast-grep",
            "choco install ast-grep",
            "npm install -g @ast-grep/cli",
        ],
        _ => &[
            "npm install -g @ast-grep/cli",
            "cargo install ast-grep --locked",
            "brew install ast-grep  # linuxbrew",
        ],
    };
    let mut hints: Vec<String> = platform_hints
        .iter()
        .map(|hint| (*hint).to_string())
        .collect();
    hints.push(OMO_PROVISION_HINT.to_string());
    hints.push(format!(
        "Or point {SG_PATH_ENV_KEY} at an existing ast-grep binary"
    ));
    hints
}

pub fn sg_binary_not_found_message(platform: &str) -> String {
    format!(
        "ast-grep binary not found for {platform}: no candidate passed the --version probe across the env override, OMO runtime, skill bin cache, PATH, or Homebrew prefixes."
    )
}

pub(crate) fn env_value(env: Option<&HashMap<String, String>>, key: &str) -> Option<String> {
    let raw = match env {
        Some(env) => env.get(key).cloned(),
        None => std::env::var(key).ok(),
    };
    raw.map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}
