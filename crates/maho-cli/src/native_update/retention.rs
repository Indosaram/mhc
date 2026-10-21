use std::fs;
use std::path::Path;

use super::{lease, pointer};

/// Removes versions outside `keep` whose lock can be taken exclusively (no running session) and stale
/// `.staging-*` workspaces. The caller must hold the update lock. Returns warnings for entries it
/// could not remove; in-use versions are skipped silently and retried by the next update.
pub(super) fn run(root: &Path, keep: &[&str]) -> Vec<String> {
    let mut warnings = Vec::new();
    match fs::read_dir(root.join("versions")) {
        Ok(entries) => {
            for entry in entries.filter_map(Result::ok) {
                let name = entry.file_name();
                let Some(version) = name.to_str() else { continue };
                if keep.contains(&version) || !pointer::is_valid_version(version) || !entry.path().is_dir() {
                    continue;
                }
                match lease::try_exclusive_version(root, version) {
                    Ok(Some(lock)) => {
                        if let Err(error) = fs::remove_dir_all(entry.path()) {
                            warnings.push(format!("cannot remove old version {version}: {error}"));
                        }
                        drop(lock);
                        if let Err(error) = fs::remove_file(lease::version_lock_path(root, version))
                            && error.kind() != std::io::ErrorKind::NotFound
                        {
                            warnings.push(format!("cannot remove lock for {version}: {error}"));
                        }
                    }
                    Ok(None) => {}
                    Err(error) => warnings.push(format!("cannot lock old version {version}: {error}")),
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => warnings.push(format!("cannot list installed versions: {error}")),
    }
    match fs::read_dir(root) {
        Ok(entries) => {
            for entry in entries.filter_map(Result::ok) {
                if entry.file_name().to_string_lossy().starts_with(".staging-")
                    && let Err(error) = fs::remove_dir_all(entry.path())
                {
                    warnings.push(format!("cannot remove stale staging {}: {error}", entry.path().display()));
                }
            }
        }
        Err(error) => warnings.push(format!("cannot list {}: {error}", root.display())),
    }
    warnings
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn skips_versions_whose_lock_is_held_shared() -> TestResult {
        let root = tempfile::tempdir()?;
        for version in ["1.0.0", "1.1.0", "1.2.0"] {
            fs::create_dir_all(root.path().join("versions").join(version))?;
        }
        fs::create_dir(root.path().join(".staging-abc"))?;
        let session = lease::SessionLease::acquire(root.path(), "1.0.0")?;

        assert!(run(root.path(), &["1.2.0"]).is_empty());
        assert!(root.path().join("versions/1.0.0").is_dir(), "in-use version must survive");
        assert!(!root.path().join("versions/1.1.0").exists());
        assert!(!lease::version_lock_path(root.path(), "1.1.0").exists());
        assert!(root.path().join("versions/1.2.0").is_dir(), "kept version must survive");
        assert!(!root.path().join(".staging-abc").exists());

        drop(session);
        assert!(run(root.path(), &["1.2.0"]).is_empty());
        assert!(!root.path().join("versions/1.0.0").exists());
        Ok(())
    }
}
