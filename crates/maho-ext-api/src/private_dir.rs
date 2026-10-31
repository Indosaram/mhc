//! Owner-only cache folders for content the permission system may trust (bundled skills).

use std::path::Path;

/// Create `dir` (mode 0700 on unix) unless it already exists as a real directory. A symlink or any
/// other non-directory in its place is refused (`None`): whoever planted it would control what is
/// written and later trusted there.
pub fn create_private_dir(dir: &Path) -> Option<()> {
    match std::fs::symlink_metadata(dir) {
        Ok(metadata) => metadata.file_type().is_dir().then_some(()),
        Err(_) => {
            let mut builder = std::fs::DirBuilder::new();
            #[cfg(unix)]
            std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
            builder.create(dir).ok()
        }
    }
}
