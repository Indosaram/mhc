use std::fs;
use std::path::Path;

/// Version removal is deferred: a launcher may have read a pointer without acquiring its lease yet.
/// Only abandoned staging directories are removed while the caller holds the update lock.
pub(super) fn run(root: &Path, _keep: &[&str]) -> Vec<String> {
    let mut warnings = Vec::new();
    match fs::read_dir(root) {
        Ok(entries) => {
            for entry in entries {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(error) => {
                        warnings.push(format!("cannot inspect staging entry: {error}"));
                        continue;
                    }
                };
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

    #[test]
    fn retains_all_packages_and_removes_abandoned_staging() -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        for version in ["1.0.0", "1.1.0", "1.2.0"] {
            fs::create_dir_all(root.path().join("versions").join(version))?;
        }
        fs::create_dir(root.path().join(".staging-abandoned"))?;
        let session = crate::native_update::install::SessionLease::acquire(root.path(), "1.0.0")?;
        assert!(run(root.path(), &["1.2.0"]).is_empty());
        for version in ["1.0.0", "1.1.0", "1.2.0"] {
            assert!(root.path().join("versions").join(version).is_dir());
        }
        assert!(!root.path().join(".staging-abandoned").exists());
        drop(session);
        assert!(run(root.path(), &["1.2.0"]).is_empty());
        assert!(root.path().join("versions/1.0.0").is_dir());
        Ok(())
    }
}
