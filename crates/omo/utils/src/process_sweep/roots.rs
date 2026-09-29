use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::codegraph::{BuildCodegraphEnvOptions, build_codegraph_env};

use super::lsp_daemon_family::resolve_home_dir;

#[derive(Default, Clone)]
pub struct CodegraphOwnedRootsOptions {
    pub codex_home: Option<PathBuf>,
    pub env: Option<HashMap<String, String>>,
    pub extra_roots: Vec<String>,
    pub home_dir: Option<PathBuf>,
    pub plugin_root: Option<String>,
    pub trusted_codegraph_install_dir: Option<String>,
}

const OMO_CODEX_PLUGIN_CACHE_PUBLISHERS: [&str; 1] = ["sisyphuslabs"];

/// OMO-owned plugin roots trusted for process matching by every sweep family:
/// the provisioned codegraph install dir, the Claude OMO install, the plugin
/// root and the trusted-publisher Codex plugin cache. An ambient
/// `CODEGRAPH_INSTALL_DIR` is deliberately NOT trusted.
pub fn discover_codegraph_owned_roots(options: &CodegraphOwnedRootsOptions) -> Vec<String> {
    let env_value = |key: &str| match &options.env {
        Some(env) => env.get(key).cloned(),
        None => std::env::var(key).ok(),
    };
    let home_dir = resolve_home_dir(options.home_dir.as_deref(), env_value);
    let mut roots: Vec<String> = Vec::new();
    add_root(&mut roots, options.trusted_codegraph_install_dir.as_deref());
    let install_dir = build_codegraph_env(&BuildCodegraphEnvOptions {
        home_dir: Some(home_dir.clone()),
        daemon: None,
    })
    .install_dir;
    add_root(&mut roots, Some(&install_dir.to_string_lossy()));
    add_root(
        &mut roots,
        Some(&home_dir.join(".claude").join("omo").to_string_lossy()),
    );
    add_root(&mut roots, options.plugin_root.as_deref());
    for root in &options.extra_roots {
        add_root(&mut roots, Some(root));
    }
    let codex_home = options
        .codex_home
        .clone()
        .or_else(|| env_value("CODEX_HOME").map(PathBuf::from))
        .unwrap_or_else(|| home_dir.join(".codex"));
    for root in read_codex_plugin_cache_roots(&codex_home) {
        add_root(&mut roots, Some(&root.to_string_lossy()));
    }
    roots
}

pub fn discover_omo_owned_roots(options: &CodegraphOwnedRootsOptions) -> Vec<String> {
    discover_codegraph_owned_roots(options)
}

fn read_codex_plugin_cache_roots(codex_home: &Path) -> Vec<PathBuf> {
    let cache_root = codex_home.join("plugins").join("cache");
    let mut roots = Vec::new();
    for publisher in read_dir_names(&cache_root) {
        if !OMO_CODEX_PLUGIN_CACHE_PUBLISHERS.contains(&publisher.as_str()) {
            continue;
        }
        let omo_root = cache_root.join(&publisher).join("omo");
        roots.extend(
            read_dir_names(&omo_root)
                .into_iter()
                .map(|version| omo_root.join(version)),
        );
    }
    roots
}

fn read_dir_names(path: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(path) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect()
}

fn add_root(roots: &mut Vec<String>, root: Option<&str>) {
    let Some(root) = root.filter(|root| !root.trim().is_empty()) else {
        return;
    };
    let resolved = std::path::absolute(root).unwrap_or_else(|_| PathBuf::from(root));
    let real = std::fs::canonicalize(&resolved).unwrap_or_else(|_| resolved.clone());
    for candidate in [resolved, real] {
        let value = candidate.to_string_lossy().into_owned();
        if !roots.contains(&value) {
            roots.push(value);
        }
    }
}
