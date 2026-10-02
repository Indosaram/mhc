use std::path::{Path, PathBuf};
use super::constants::PROJECT_MARKERS;

pub fn find_project_root(start_path: &Path, markers: Option<&[&str]>) -> Option<PathBuf> {
    find_project_root_with_stat(start_path, markers, |path| path.metadata())
}

fn find_project_root_with_stat(start_path: &Path, markers: Option<&[&str]>, stat: impl FnOnce(&Path) -> std::io::Result<std::fs::Metadata>) -> Option<PathBuf> {
    let resolved = start_path.canonicalize().ok()?;
    let metadata = stat(&resolved).ok()?;
    let mut current = if metadata.is_dir() { resolved } else { resolved.parent()?.to_owned() };
    loop {
        if markers.unwrap_or(PROJECT_MARKERS).iter().any(|marker| current.join(marker).exists()) { return Some(current); }
        current = current.parent()?.to_owned();
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn stat_failure_after_canonicalization_returns_none() {
        let root = tempfile::tempdir().expect("temp directory");
        std::fs::create_dir(root.path().join(".git")).expect("marker");
        let result = super::find_project_root_with_stat(root.path(), None, |path| {
            assert_eq!(path, root.path().canonicalize().expect("canonical path"));
            Err(std::io::Error::from(std::io::ErrorKind::NotFound))
        });
        assert_eq!(result, None);
    }
}
