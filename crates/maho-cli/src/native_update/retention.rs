use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

/// Removes abandoned `.staging-*` directories and version packages that are not current, previous,
/// or held by a live session lease. A held version stays until a later update.
///
/// Current and previous are kept even with no lease: a launcher may have read the pointer and not
/// acquired `versions/<v>.lock` yet. `keep` is unioned with `current.json` when that file is valid.
/// An unreadable pointer skips version removal so current and previous cannot be guessed wrong.
pub(super) fn run(root: &Path, keep: &[&str]) -> Vec<String> {
    let mut warnings = Vec::new();
    remove_abandoned_staging(root, &mut warnings);
    let Some(protected) = protected_versions(root, keep, &mut warnings) else {
        return warnings;
    };
    remove_unused_versions(root, &protected, &mut warnings);
    warnings
}

fn remove_abandoned_staging(root: &Path, warnings: &mut Vec<String>) {
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
}

fn protected_versions(root: &Path, keep: &[&str], warnings: &mut Vec<String>) -> Option<BTreeSet<String>> {
    let mut protected: BTreeSet<String> = keep.iter().copied().map(|version| version.to_owned()).collect();
    let pointer_path = root.join(super::pointer::FILE_NAME);
    if !pointer_path.is_file() {
        return Some(protected);
    }
    match super::pointer::read(root) {
        Ok(pointer) => {
            protected.insert(pointer.current);
            if let Some(previous) = pointer.previous {
                protected.insert(previous);
            }
            Some(protected)
        }
        Err(error) => {
            warnings.push(format!(
                "cannot read version pointer; leaving installed versions in place: {error}"
            ));
            None
        }
    }
}

fn remove_unused_versions(root: &Path, protected: &BTreeSet<String>, warnings: &mut Vec<String>) {
    let versions = root.join("versions");
    let entries = match fs::read_dir(&versions) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => {
            warnings.push(format!("cannot list {}: {error}", versions.display()));
            return;
        }
    };
    let mut candidates = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                warnings.push(format!("cannot inspect version entry: {error}"));
                continue;
            }
        };
        let Ok(name) = entry.file_name().into_string() else { continue };
        if !super::pointer::is_valid_version(&name) || protected.contains(&name) {
            continue;
        }
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => candidates.push(name),
            Ok(_) => {}
            Err(error) => warnings.push(format!("cannot inspect version {name}: {error}")),
        }
    }
    for name in candidates {
        let directory = versions.join(&name);
        match super::lease::try_exclusive_version(root, &name) {
            Ok(Some(lock)) => {
                if let Err(error) = fs::remove_dir_all(&directory)
                    && error.kind() != std::io::ErrorKind::NotFound
                {
                    warnings.push(format!("cannot remove version {}: {error}", directory.display()));
                }
                drop(lock);
            }
            Ok(None) => {}
            Err(error) => {
                warnings.push(format!("cannot check whether version {name} is in use: {error}"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_unheld_versions_and_defers_a_live_lease() -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        for version in ["1.0.0", "1.1.0", "1.2.0"] {
            fs::create_dir_all(root.path().join("versions").join(version))?;
        }
        fs::create_dir(root.path().join(".staging-abandoned"))?;
        let session = crate::native_update::install::SessionLease::acquire(root.path(), "1.0.0")?;
        assert!(run(root.path(), &["1.2.0"]).is_empty());
        assert!(root.path().join("versions/1.0.0").is_dir(), "live lease defers cleanup");
        assert!(!root.path().join("versions/1.1.0").exists(), "unheld version is removed");
        assert!(root.path().join("versions/1.2.0").is_dir(), "kept version stays");
        assert!(!root.path().join(".staging-abandoned").exists());
        drop(session);
        assert!(run(root.path(), &["1.2.0"]).is_empty());
        assert!(!root.path().join("versions/1.0.0").exists(), "released version is removed on the next update");
        assert!(root.path().join("versions/1.2.0").is_dir());
        Ok(())
    }

    #[test]
    fn keeps_pointer_versions_and_skips_removal_when_the_pointer_is_unreadable() -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        for version in ["1.0.0", "1.1.0", "1.2.0"] {
            fs::create_dir_all(root.path().join("versions").join(version))?;
        }
        super::super::pointer::publish(
            root.path(),
            &super::super::pointer::Pointer::new("1.2.0".into(), Some("1.1.0".into())),
        )?;
        assert!(run(root.path(), &[]).is_empty());
        assert!(root.path().join("versions/1.2.0").is_dir(), "current stays");
        assert!(root.path().join("versions/1.1.0").is_dir(), "previous stays");
        assert!(!root.path().join("versions/1.0.0").exists());

        fs::create_dir_all(root.path().join("versions/1.0.0"))?;
        fs::write(root.path().join(super::super::pointer::FILE_NAME), b"not json")?;
        let warnings = run(root.path(), &[]);
        assert!(warnings.iter().any(|warning| warning.contains("version pointer")), "{warnings:?}");
        assert!(root.path().join("versions/1.0.0").is_dir(), "unreadable pointer blocks version removal");
        assert!(root.path().join("versions/1.2.0").is_dir());
        Ok(())
    }
}
