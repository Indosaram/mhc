use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use super::managed_runtime::{
    FileExistsFn, ResolvePinnedCodegraphBinOptions, resolve_pinned_codegraph_bin,
};
use super::node_support::{
    CODEGRAPH_NODE_BIN_ENV, CodegraphNodeSupport, EvaluateCodegraphNodeSupportOptions,
    evaluate_codegraph_node_support,
};
use crate::runtime::bun_which;

pub const CODEGRAPH_PACKAGE: &str = "@colbymchenry/codegraph";
pub const CODEGRAPH_ENV_BIN: &str = "OMO_CODEGRAPH_BIN";
pub const CODEGRAPH_LEGACY_ENV_BIN: &str = "CODEGRAPH_BIN";

const CODEGRAPH_NODE_CANDIDATES: &[&str] = &["node24", "node22", "node20", "node"];
const CODEGRAPH_NODE_PATH_CANDIDATES: &[&str] = &[
    "/opt/homebrew/opt/node@24/bin/node",
    "/opt/homebrew/opt/node@22/bin/node",
    "/opt/homebrew/opt/node@20/bin/node",
    "/usr/local/opt/node@24/bin/node",
    "/usr/local/opt/node@22/bin/node",
    "/usr/local/opt/node@20/bin/node",
];

pub type NodeVersionFn<'a> = &'a dyn Fn(&str) -> Option<String>;
pub type RequireResolveFn<'a> = &'a dyn Fn(&str) -> Option<PathBuf>;
pub type WhichFn<'a> = &'a dyn Fn(&str) -> Option<PathBuf>;
pub type NodeRuntimeFn<'a> = &'a dyn Fn() -> Option<String>;
pub type ProvisionedFn<'a> = &'a dyn Fn() -> Option<PathBuf>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CodegraphCommandSource {
    Bundled,
    Env,
    Path,
    Provisioned,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodegraphCommandResolution {
    pub args_prefix: Vec<String>,
    pub command: String,
    pub exists: bool,
    pub source: CodegraphCommandSource,
}

pub fn codegraph_command_requires_supported_local_node(
    resolution: &CodegraphCommandResolution,
) -> bool {
    resolution.source != CodegraphCommandSource::Bundled
        && resolution.source != CodegraphCommandSource::Env
        && resolution.source != CodegraphCommandSource::Provisioned
}

#[derive(Default)]
pub struct ResolveCodegraphCommandOptions<'a> {
    pub env: Option<BTreeMap<String, String>>,
    pub file_exists: Option<FileExistsFn<'a>>,
    pub home_dir: Option<PathBuf>,
    pub node_runtime: Option<NodeRuntimeFn<'a>>,
    pub node_version: Option<NodeVersionFn<'a>>,
    pub provisioned: Option<ProvisionedFn<'a>>,
    pub require_resolve: Option<RequireResolveFn<'a>>,
    pub which: Option<WhichFn<'a>>,
}

#[derive(Default)]
pub struct ResolveCodegraphNodeSupportOptions<'a> {
    pub env: Option<BTreeMap<String, String>>,
    pub file_exists: Option<FileExistsFn<'a>>,
    pub node_version: Option<NodeVersionFn<'a>>,
    pub which: Option<WhichFn<'a>>,
}

fn default_node_version(node_path: &str) -> Option<String> {
    let output = Command::new(node_path)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}\n{stderr}");
    let first = combined.split_whitespace().next()?;
    if first.is_empty() {
        None
    } else {
        Some(first.to_string())
    }
}

fn is_node_executable_name(file_path: &Path) -> bool {
    let Some(name) = file_path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let lower = name.to_lowercase();
    if lower == "node" || lower == "node.exe" {
        return true;
    }
    if let Some(rest) = lower.strip_prefix("node") {
        let rest = rest.strip_suffix(".exe").unwrap_or(rest);
        return !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit());
    }
    false
}

fn looks_like_path(command: &str) -> bool {
    command.contains('/')
        || command.contains('\\')
        || (command.len() >= 2
            && command.as_bytes()[0].is_ascii_alphabetic()
            && command.as_bytes()[1] == b':')
}

fn supports_codegraph_node_runtime(
    node_path: &str,
    env: Option<&BTreeMap<String, String>>,
    node_version: &dyn Fn(&str) -> Option<String>,
) -> bool {
    let Some(version) = node_version(node_path) else {
        return false;
    };
    let support = evaluate_codegraph_node_support(&EvaluateCodegraphNodeSupportOptions {
        env: env.cloned(),
        node_version: Some(version),
    });
    support.supported
}

fn default_node_runtime(
    env: Option<&BTreeMap<String, String>>,
    file_exists: &dyn Fn(&Path) -> bool,
    which: &dyn Fn(&str) -> Option<PathBuf>,
    node_version: &dyn Fn(&str) -> Option<String>,
) -> Option<String> {
    let configured = env
        .and_then(|e| e.get(CODEGRAPH_NODE_BIN_ENV))
        .map(|s| s.trim().to_string())
        .or_else(|| {
            std::env::var(CODEGRAPH_NODE_BIN_ENV)
                .ok()
                .map(|s| s.trim().to_string())
        })
        .filter(|s| !s.is_empty());

    if let Some(conf) = configured {
        let resolved = if looks_like_path(&conf) {
            if file_exists(Path::new(&conf)) {
                Some(conf)
            } else {
                None
            }
        } else {
            which(&conf).map(|p| p.to_string_lossy().into_owned())
        };
        if let Some(r) = resolved
            && supports_codegraph_node_runtime(&r, env, node_version)
        {
            return Some(r);
        }
        return None;
    }

    let mut candidates = Vec::new();
    if let Ok(current_exe) = std::env::current_exe()
        && is_node_executable_name(&current_exe)
    {
        candidates.push(current_exe.to_string_lossy().into_owned());
    }
    for candidate_name in CODEGRAPH_NODE_CANDIDATES {
        if let Some(p) = which(candidate_name) {
            candidates.push(p.to_string_lossy().into_owned());
        }
    }
    for candidate_path in CODEGRAPH_NODE_PATH_CANDIDATES {
        if file_exists(Path::new(candidate_path)) {
            candidates.push((*candidate_path).to_string());
        }
    }

    let mut seen = BTreeSet::new();
    candidates.into_iter().find(|candidate| {
        seen.insert(candidate.clone())
            && supports_codegraph_node_runtime(candidate, env, node_version)
    })
}

pub fn resolve_codegraph_node_runtime(
    options: &ResolveCodegraphNodeSupportOptions<'_>,
) -> Option<String> {
    let env = options.env.as_ref();
    let default_file_exists = |p: &Path| p.exists();
    let file_exists: &dyn Fn(&Path) -> bool = options.file_exists.unwrap_or(&default_file_exists);
    let default_which = |name: &str| bun_which(name);
    let which: &dyn Fn(&str) -> Option<PathBuf> = options.which.unwrap_or(&default_which);
    let default_node_ver = |p: &str| default_node_version(p);
    let node_version: &dyn Fn(&str) -> Option<String> =
        options.node_version.unwrap_or(&default_node_ver);

    default_node_runtime(env, file_exists, which, node_version)
}

pub fn resolve_codegraph_node_support(
    options: &ResolveCodegraphNodeSupportOptions<'_>,
) -> CodegraphNodeSupport {
    let default_node_ver = |p: &str| default_node_version(p);
    let node_version: &dyn Fn(&str) -> Option<String> =
        options.node_version.unwrap_or(&default_node_ver);

    let runtime = resolve_codegraph_node_runtime(options);
    let ver_str = match runtime {
        Some(r) => node_version(&r).unwrap_or_else(|| "0.0.0".to_string()),
        None => "0.0.0".to_string(),
    };

    evaluate_codegraph_node_support(&EvaluateCodegraphNodeSupportOptions {
        env: options.env.clone(),
        node_version: Some(ver_str),
    })
}

fn resolve_bundled_shim(
    require_resolve: Option<RequireResolveFn<'_>>,
    file_exists: &dyn Fn(&Path) -> bool,
) -> Option<String> {
    let resolver = require_resolve?;
    let package_json = resolver(&format!("{CODEGRAPH_PACKAGE}/package.json"))?;
    let package_root = package_json.parent()?;
    let candidates = [
        package_root.join("bin").join("codegraph.js"),
        package_root.join("npm-shim.js"),
    ];
    for candidate in candidates {
        if file_exists(&candidate) {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    None
}

pub fn resolve_codegraph_command(
    options: &ResolveCodegraphCommandOptions<'_>,
) -> CodegraphCommandResolution {
    let env = options.env.as_ref();
    let default_file_exists = |p: &Path| p.exists();
    let file_exists: &dyn Fn(&Path) -> bool = options.file_exists.unwrap_or(&default_file_exists);

    let configured_bin = env
        .and_then(|e| {
            e.get(CODEGRAPH_ENV_BIN)
                .or_else(|| e.get(CODEGRAPH_LEGACY_ENV_BIN))
        })
        .map(|s| s.trim().to_string())
        .or_else(|| {
            std::env::var(CODEGRAPH_ENV_BIN)
                .or_else(|_| std::env::var(CODEGRAPH_LEGACY_ENV_BIN))
                .ok()
                .map(|s| s.trim().to_string())
        })
        .filter(|s| !s.is_empty());

    if let Some(bin) = configured_bin {
        let exists = file_exists(Path::new(&bin));
        return CodegraphCommandResolution {
            args_prefix: Vec::new(),
            command: bin,
            exists,
            source: CodegraphCommandSource::Env,
        };
    }

    let default_which = |name: &str| bun_which(name);
    let which: &dyn Fn(&str) -> Option<PathBuf> = options.which.unwrap_or(&default_which);

    let default_node_ver = |p: &str| default_node_version(p);
    let node_version: &dyn Fn(&str) -> Option<String> =
        options.node_version.unwrap_or(&default_node_ver);

    let get_node_runtime = || -> Option<String> {
        if let Some(f) = options.node_runtime {
            f()
        } else {
            default_node_runtime(env, file_exists, which, node_version)
        }
    };

    let bundled = resolve_bundled_shim(options.require_resolve, file_exists);
    let runtime = get_node_runtime();
    if let (Some(b), Some(r)) = (bundled, runtime) {
        return CodegraphCommandResolution {
            args_prefix: vec![b],
            command: r,
            exists: true,
            source: CodegraphCommandSource::Bundled,
        };
    }

    let provisioned = if let Some(p_fn) = options.provisioned {
        p_fn()
    } else {
        let home = options
            .home_dir
            .clone()
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from(""));
        let install_dir = home.join(".maho").join("codegraph");
        resolve_pinned_codegraph_bin(
            Some(&install_dir),
            &ResolvePinnedCodegraphBinOptions {
                file_exists: Some(file_exists),
                platform: None,
                read_text: None,
            },
        )
    };

    if let Some(prov) = provisioned
        && file_exists(&prov)
    {
        return CodegraphCommandResolution {
            args_prefix: Vec::new(),
            command: prov.to_string_lossy().into_owned(),
            exists: true,
            source: CodegraphCommandSource::Provisioned,
        };
    }

    let path_command = which("codegraph");
    let (cmd, exists) = match path_command {
        Some(p) => (p.to_string_lossy().into_owned(), true),
        None => ("codegraph".to_string(), false),
    };

    CodegraphCommandResolution {
        args_prefix: Vec::new(),
        command: cmd,
        exists,
        source: CodegraphCommandSource::Path,
    }
}
