use std::path::{Path, PathBuf};

use crate::backend::Result;
use crate::util::{device_of, dirname, is_at_or_below, is_denied, mkdtemp_in, resolve_path, sha1_hex};

pub trait BaseDirIo {
    fn stat_dev(&self, path: &Path) -> Result<u64>;
    fn writable(&self, path: &Path) -> Result<bool>;
}

pub struct FilesystemBaseDirIo;

impl BaseDirIo for FilesystemBaseDirIo {
    fn stat_dev(&self, path: &Path) -> Result<u64> {
        Ok(device_of(&std::fs::metadata(path)?))
    }

    fn writable(&self, path: &Path) -> Result<bool> {
        if let Err(error) = std::fs::create_dir_all(path) {
            if is_denied(&error) {
                return Ok(false);
            }
            return Err(error.into());
        }
        let probe = mkdtemp_in(path, ".probe-")?;
        std::fs::remove_dir(&probe)?;
        Ok(true)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseDirSelection {
    pub base_dir: PathBuf,
    pub cross_device: bool,
}

pub fn choose_base_dir(
    repo_root: &Path,
    home_dir: &Path,
    id: &str,
    io: &dyn BaseDirIo,
) -> Result<BaseDirSelection> {
    let segment = format!(
        "t{}",
        &sha1_hex(&format!("{}{}", repo_root.to_string_lossy(), id))[..10]
    );
    let home_base = home_dir.join(".omo").join("wt").join(&segment);
    let repo_device = io.stat_dev(repo_root)?;
    let mut home_parent = home_dir.join(".omo");
    let home_device = loop {
        match io.stat_dev(&home_parent) {
            Ok(device) => break device,
            Err(error) if error.is_not_found() => {
                let parent = dirname(&home_parent);
                if parent == home_parent {
                    return Err(error);
                }
                home_parent = parent;
            }
            Err(error) => return Err(error),
        }
    };
    if repo_device == home_device {
        return Ok(BaseDirSelection {
            base_dir: home_base,
            cross_device: false,
        });
    }
    let mut volume_root = resolve_path(repo_root);
    let mut reference_device = repo_device;
    // A subvolume or bind-mount repository root reports its own st_dev, so the
    // same-device walk would stop AT the repository itself and place the sandbox
    // inside the very tree the backends clone. When the parent is writable, the
    // repository is nested inside the parent's filesystem (btrfs subvolume, zfs
    // dataset): adopt the parent's device and continue the walk from there.
    {
        let parent = dirname(&volume_root);
        if parent != volume_root {
            let parent_stat = io.stat_dev(&parent)?;
            if parent_stat != repo_device && io.writable(&parent)? {
                volume_root = parent;
                reference_device = parent_stat;
            }
        }
    }
    while dirname(&volume_root) != volume_root {
        let parent = dirname(&volume_root);
        if io.stat_dev(&parent)? != reference_device {
            break;
        }
        volume_root = parent;
    }
    let volume_base = volume_root.join(".omo-wt");
    // Hard boundary: whatever the walk produced, the sandbox must never live
    // inside the repository being isolated.
    let repository = resolve_path(repo_root);
    let inside_repository = is_at_or_below(&volume_base, &repository);
    if !inside_repository && io.writable(&volume_base)? {
        return Ok(BaseDirSelection {
            base_dir: volume_base.join(&segment),
            cross_device: false,
        });
    }
    Ok(BaseDirSelection {
        base_dir: home_base,
        cross_device: true,
    })
}
