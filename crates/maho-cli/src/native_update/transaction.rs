use std::ffi::OsString;
use std::fs;
use std::path::Path;

#[cfg(any(not(windows), test))]
pub fn replace(
    installation: &Path,
    candidate: &Path,
    backup: &Path,
    validate: impl FnOnce(&Path) -> Result<(), String>,
) -> Result<(), String> {
    replace_retaining_backup(installation, candidate, backup, validate)?;
    fs::remove_dir_all(backup).map_err(|error| {
        format!("update installed, but backup cleanup failed at {}: {error}", backup.display())
    })?;
    Ok(())
}

#[cfg(any(not(windows), test))]
pub fn replace_retaining_backup(
    installation: &Path,
    candidate: &Path,
    backup: &Path,
    validate: impl FnOnce(&Path) -> Result<(), String>,
) -> Result<(), String> {
    if backup.exists() {
        return Err(format!("backup already exists: {}", backup.display()));
    }
    fs::rename(installation, backup)
        .map_err(|error| format!("cannot back up installation: {error}"))?;
    if let Err(error) = fs::rename(candidate, installation) {
        return match fs::rename(backup, installation) {
            Ok(()) => Err(format!("cannot install candidate; previous installation restored: {error}")),
            Err(restore) => Err(format!(
                "cannot install candidate: {error}; restore failed: {restore}; backup retained at {}",
                backup.display()
            )),
        };
    }
    if let Err(error) = validate(installation) {
        fs::rename(installation, candidate).map_err(|restore| {
            format!(
                "candidate validation failed: {error}; cannot move failed candidate: {restore}; backup retained at {}",
                backup.display()
            )
        })?;
        fs::rename(backup, installation).map_err(|restore| {
            format!(
                "candidate validation failed: {error}; restore failed: {restore}; backup retained at {}",
                backup.display()
            )
        })?;
        return Err(format!("candidate validation failed; previous installation restored: {error}"));
    }
    Ok(())
}

#[cfg(any(windows, test))]
pub fn replace_entries_retaining_backup(
    installation: &Path,
    candidate: &Path,
    backup: &Path,
    validate: impl FnOnce(&Path) -> Result<(), String>,
) -> Result<(), String> {
    if backup.exists() {
        return Err(format!("backup already exists: {}", backup.display()));
    }
    fs::create_dir(backup).map_err(|error| format!("cannot create backup directory: {error}"))?;

    let install_entries = match fs::read_dir(installation) {
        Ok(read) => {
            let mut names = Vec::new();
            let mut err = None;
            for entry in read {
                match entry {
                    Ok(entry) => names.push(entry.file_name()),
                    Err(error) => {
                        err = Some(error.to_string());
                        break;
                    }
                }
            }
            if let Some(error) = err {
                let _ = fs::remove_dir(backup);
                return Err(format!("cannot read installation entries: {error}"));
            }
            names
        }
        Err(error) => {
            let _ = fs::remove_dir(backup);
            return Err(format!("cannot read installation directory: {error}"));
        }
    };

    let mut backed_up: Vec<OsString> = Vec::new();
    for name in install_entries {
        if let Err(error) = fs::rename(installation.join(&name), backup.join(&name)) {
            for restored in backed_up {
                if let Err(restore) = fs::rename(backup.join(&restored), installation.join(&restored)) {
                    return Err(format!(
                        "cannot back up installation: {error}; restore failed: {restore}; backup retained at {}",
                        backup.display()
                    ));
                }
            }
            let _ = fs::remove_dir(backup);
            return Err(format!("cannot back up installation: {error}"));
        }
        backed_up.push(name);
    }

    let candidate_entries = match fs::read_dir(candidate) {
        Ok(read) => {
            let mut names = Vec::new();
            let mut err = None;
            for entry in read {
                match entry {
                    Ok(entry) => names.push(entry.file_name()),
                    Err(error) => {
                        err = Some(error.to_string());
                        break;
                    }
                }
            }
            match err {
                Some(error) => Err(format!("cannot read candidate entries: {error}")),
                None => Ok(names),
            }
        }
        Err(error) => Err(format!("cannot read candidate directory: {error}")),
    };

    let restore_all = |installed_from_candidate: Vec<OsString>,
                       backed_up: Vec<OsString>,
                       prefix: &str,
                       error: String|
     -> Result<(), String> {
        for restored in installed_from_candidate {
            if let Err(restore) = fs::rename(installation.join(&restored), candidate.join(&restored)) {
                return Err(format!(
                    "{prefix}: {error}; cannot move failed candidate: {restore}; backup retained at {}",
                    backup.display()
                ));
            }
        }
        for restored in backed_up {
            if let Err(restore) = fs::rename(backup.join(&restored), installation.join(&restored)) {
                return Err(format!(
                    "{prefix}: {error}; restore failed: {restore}; backup retained at {}",
                    backup.display()
                ));
            }
        }
        let _ = fs::remove_dir(backup);
        Err(format!("{prefix}; previous installation restored: {error}"))
    };

    let candidate_entries = match candidate_entries {
        Ok(names) => names,
        Err(error) => {
            return restore_all(Vec::new(), backed_up, "cannot install candidate", error);
        }
    };

    let mut installed_from_candidate: Vec<OsString> = Vec::new();
    for name in candidate_entries {
        if let Err(error) = fs::rename(candidate.join(&name), installation.join(&name)) {
            return restore_all(installed_from_candidate, backed_up, "cannot install candidate", error.to_string());
        }
        installed_from_candidate.push(name);
    }

    if let Err(error) = validate(installation) {
        return restore_all(installed_from_candidate, backed_up, "candidate validation failed", error);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package(root: &Path, name: &str, bytes: &[u8]) -> std::io::Result<std::path::PathBuf> {
        let path = root.join(name);
        fs::create_dir_all(path.join("skills"))?;
        fs::write(path.join("mhc"), bytes)?;
        fs::write(path.join("skills/skill.md"), bytes)?;
        Ok(path)
    }

    #[test]
    fn validation_failure_restores_binary_and_assets() -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let installed = package(root.path(), "installed", b"old")?;
        let candidate = package(root.path(), "candidate", b"new")?;
        let backup = root.path().join("backup");
        let result = replace(&installed, &candidate, &backup, |live| {
            assert_eq!(fs::read(live.join("mhc")).expect("candidate binary"), b"new");
            Err("unusable candidate".to_owned())
        });
        assert!(result.is_err());
        assert_eq!(fs::read(installed.join("mhc"))?, b"old");
        assert_eq!(fs::read(installed.join("skills/skill.md"))?, b"old");
        assert!(!backup.exists());
        Ok(())
    }

    #[test]
    fn missing_candidate_restores_after_backup_rename() -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let installed = package(root.path(), "installed", b"old")?;
        let result = replace(&installed, &root.path().join("missing"), &root.path().join("backup"), |_| {
            panic!("validation cannot run without candidate")
        });
        assert!(result.is_err());
        assert_eq!(fs::read(installed.join("mhc"))?, b"old");
        Ok(())
    }

    #[test]
    fn successful_replacement_removes_backup() -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let installed = package(root.path(), "installed", b"old")?;
        let candidate = package(root.path(), "candidate", b"new")?;
        let backup = root.path().join("backup");
        replace(&installed, &candidate, &backup, |_| Ok(()))?;
        assert_eq!(fs::read(installed.join("skills/skill.md"))?, b"new");
        assert!(!backup.exists());
        assert!(!candidate.exists());
        Ok(())
    }

    #[test]
    #[test]
    fn retained_backup_contains_complete_previous_package() -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let installed = package(root.path(), "installed", b"old")?;
        let candidate = package(root.path(), "candidate", b"new")?;
        let backup = root.path().join("backup");
        replace_retaining_backup(&installed, &candidate, &backup, |_| Ok(()))?;
        assert_eq!(fs::read(installed.join("mhc"))?, b"new");
        assert_eq!(fs::read(backup.join("mhc"))?, b"old");
        assert_eq!(fs::read(backup.join("skills/skill.md"))?, b"old");
        Ok(())
    }

    #[test]
    fn replace_entries_retaining_backup_succeeds() -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let installed = package(root.path(), "installed", b"old")?;
        let candidate = package(root.path(), "candidate", b"new")?;
        let backup = root.path().join("backup");
        replace_entries_retaining_backup(&installed, &candidate, &backup, |_| Ok(()))?;
        assert_eq!(fs::read(installed.join("mhc"))?, b"new");
        assert_eq!(fs::read(installed.join("skills/skill.md"))?, b"new");
        assert_eq!(fs::read(backup.join("mhc"))?, b"old");
        assert_eq!(fs::read(backup.join("skills/skill.md"))?, b"old");
        assert!(backup.is_dir());
        Ok(())
    }

    #[test]
    fn replace_entries_retaining_backup_validation_failure_restores() -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let installed = package(root.path(), "installed", b"old")?;
        let candidate = package(root.path(), "candidate", b"new")?;
        let backup = root.path().join("backup");
        let result = replace_entries_retaining_backup(&installed, &candidate, &backup, |_| {
            Err("validation failed".to_string())
        });
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("previous installation restored"));
        assert_eq!(fs::read(installed.join("mhc"))?, b"old");
        assert_eq!(fs::read(installed.join("skills/skill.md"))?, b"old");
        assert_eq!(fs::read_dir(&installed)?.count(), 2);
        assert!(!backup.exists());
        Ok(())
    }

    #[test]
    fn replace_entries_retaining_backup_missing_candidate_preserves_package() -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let installed = package(root.path(), "installed", b"old")?;
        let missing = root.path().join("missing");
        let backup = root.path().join("backup");
        let result = replace_entries_retaining_backup(&installed, &missing, &backup, |_| {
            panic!("validation cannot run without candidate")
        });
        assert!(result.is_err());
        assert_eq!(fs::read(installed.join("mhc"))?, b"old");
        assert_eq!(fs::read(installed.join("skills/skill.md"))?, b"old");
        assert_eq!(fs::read_dir(&installed)?.count(), 2);
        assert!(!backup.exists());
        Ok(())
    }
}
