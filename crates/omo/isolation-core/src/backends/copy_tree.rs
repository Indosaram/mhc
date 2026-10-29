use std::path::Path;
use std::sync::Arc;

use crate::backend::{IsolationError, Result};
use crate::util::{is_at_or_below, resolve_path, symlink_at};

pub const DEFAULT_MAX_COPY_BYTES: u64 = 2 * 1024 * 1024 * 1024;

pub type SpaceFn = Arc<dyn Fn(&Path) -> Result<(u64, u64)> + Send + Sync>;

#[cfg(unix)]
fn default_space(path: &Path) -> Result<(u64, u64)> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|error| IsolationError::other(error.to_string()))?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok((stat.f_bavail as u64, stat.f_bsize as u64))
}

#[cfg(not(unix))]
fn default_space(_path: &Path) -> Result<(u64, u64)> {
    Ok((u64::MAX, 1))
}

pub struct CopyBudget {
    available: u64,
    max: u64,
    bytes: u64,
}

impl CopyBudget {
    pub fn consume(&mut self, size: u64) -> Result<()> {
        self.bytes += size;
        if self.bytes > self.max {
            return Err(IsolationError::unavailable(format!(
                "copy size {} bytes exceeds maxCopyBytes {}",
                self.bytes, self.max
            )));
        }
        if self.bytes + self.bytes.div_ceil(10) > self.available {
            return Err(IsolationError::unavailable("insufficient free space"));
        }
        Ok(())
    }
}

pub fn copy_budget(base: &Path, max_bytes: Option<u64>, space: Option<SpaceFn>) -> Result<CopyBudget> {
    let (bavail, bsize) = match &space {
        Some(space) => space(base)?,
        None => default_space(base)?,
    };
    Ok(CopyBudget {
        available: bavail.saturating_mul(bsize),
        max: max_bytes.unwrap_or(DEFAULT_MAX_COPY_BYTES),
        bytes: 0,
    })
}

pub fn apply_metadata(path: &Path, info: &std::fs::Metadata) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(info.permissions().mode()))?;
    }
    let atime = filetime::FileTime::from_last_access_time(info);
    let mtime = filetime::FileTime::from_last_modification_time(info);
    filetime::set_file_times(path, atime, mtime)?;
    Ok(())
}

fn apply_symlink_metadata(path: &Path, info: &std::fs::Metadata) -> Result<()> {
    let atime = filetime::FileTime::from_last_access_time(info);
    let mtime = filetime::FileTime::from_last_modification_time(info);
    filetime::set_symlink_file_times(path, atime, mtime)?;
    Ok(())
}

pub fn copy_tree(
    source: &Path,
    destination: &Path,
    clone: &mut dyn FnMut(&Path, &Path, u64) -> Result<()>,
    consume: &mut dyn FnMut(u64) -> Result<()>,
) -> Result<()> {
    // The volume walk can place the destination inside the source (a subvolume
    // repository root reports its own st_dev). Never descend into the tree this
    // copy is itself producing, or the walk chases its own output forever.
    let skip_root = resolve_path(destination);
    copy_tree_skipping(source, destination, clone, consume, &skip_root)
}

fn copy_tree_skipping(
    source: &Path,
    destination: &Path,
    clone: &mut dyn FnMut(&Path, &Path, u64) -> Result<()>,
    consume: &mut dyn FnMut(u64) -> Result<()>,
    skip_root: &Path,
) -> Result<()> {
    let info = std::fs::symlink_metadata(source)?;
    if info.file_type().is_symlink() {
        symlink_at(&std::fs::read_link(source)?, destination, source)?;
        apply_symlink_metadata(destination, &info)?;
        return Ok(());
    }
    if info.is_dir() {
        std::fs::create_dir(destination)?;
        for entry in std::fs::read_dir(source)? {
            let entry = entry?;
            let child = entry.path();
            if is_at_or_below(&child, skip_root) {
                continue;
            }
            copy_tree_skipping(
                &child,
                &destination.join(entry.file_name()),
                clone,
                consume,
                skip_root,
            )?;
        }
    } else if info.is_file() {
        consume(info.len())?;
        clone(source, destination, info.len())?;
    } else {
        return Ok(());
    }
    apply_metadata(destination, &info)?;
    Ok(())
}
