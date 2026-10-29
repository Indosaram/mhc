use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::manifest::CODEGRAPH_PINNED_VERSION;
use crate::runtime::node_platform;

pub type ReadTextFn<'a> = &'a dyn Fn(&Path) -> std::io::Result<String>;
pub type FileExistsFn<'a> = &'a dyn Fn(&Path) -> bool;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvisionMarker {
    pub bin_path: String,
    pub version: String,
}

#[derive(Default)]
pub struct ResolvePinnedCodegraphBinOptions<'a> {
    pub file_exists: Option<FileExistsFn<'a>>,
    pub platform: Option<&'a str>,
    pub read_text: Option<ReadTextFn<'a>>,
}

pub fn managed_bin_path(install_dir: &Path, platform: &str) -> PathBuf {
    let name = if platform == "win32" {
        "codegraph.cmd"
    } else {
        "codegraph"
    };
    install_dir.join("bin").join(name)
}

pub fn has_codegraph_managed_install(
    install_dir: &Path,
    options: &ResolvePinnedCodegraphBinOptions<'_>,
) -> bool {
    let platform = match options.platform {
        Some(p) => p,
        None => node_platform(),
    };
    let bin = managed_bin_path(install_dir, platform);
    let marker_dir = install_dir.join(".provisioned");
    let check_exists = |p: &Path| -> bool {
        if let Some(f) = options.file_exists {
            f(p)
        } else {
            p.exists()
        }
    };
    check_exists(&bin) || check_exists(&marker_dir)
}

fn resolve_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    }
}

pub fn resolve_pinned_codegraph_bin(
    install_dir: Option<&Path>,
    options: &ResolvePinnedCodegraphBinOptions<'_>,
) -> Option<PathBuf> {
    let install_dir = install_dir?;
    let platform = match options.platform {
        Some(p) => p,
        None => node_platform(),
    };
    let expected_bin = managed_bin_path(install_dir, platform);
    let marker_path = install_dir
        .join(".provisioned")
        .join(format!("codegraph-{CODEGRAPH_PINNED_VERSION}.json"));

    let check_exists = |p: &Path| -> bool {
        if let Some(f) = options.file_exists {
            f(p)
        } else {
            p.exists()
        }
    };

    if !check_exists(&expected_bin) || !check_exists(&marker_path) {
        return None;
    }

    let marker_text = if let Some(reader) = options.read_text {
        reader(&marker_path).ok()?
    } else {
        fs::read_to_string(&marker_path).ok()?
    };

    let marker: ProvisionMarker = serde_json::from_str(&marker_text).ok()?;
    if marker.version != CODEGRAPH_PINNED_VERSION {
        return None;
    }

    let marker_bin = PathBuf::from(&marker.bin_path);
    if resolve_path(&marker_bin) == resolve_path(&expected_bin) {
        Some(expected_bin)
    } else {
        None
    }
}
