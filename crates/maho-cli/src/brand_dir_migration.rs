use std::{
    fs, io,
    path::{Path, PathBuf},
};
pub const MIGRATION_MARKER: &str = ".migrated-from-senpi";
fn is_desktop_reserved(name: &str) -> bool {
    let folded = if cfg!(any(target_os = "macos", target_os = "windows")) {
        name.to_ascii_lowercase()
    } else {
        name.to_owned()
    };
    folded == "desktop" || folded.starts_with("desktop.init-")
}
pub struct BrandDirMigrationResult {
    pub migrated: bool,
    pub copied: Vec<String>,
    pub from: PathBuf,
    pub to: PathBuf,
}
fn copy_tree(source: &Path, target: &Path) -> io::Result<()> {
    #[cfg(unix)]
    if fs::symlink_metadata(source)?.file_type().is_symlink() {
        let link = fs::read_link(source)?;
        let link = if link.is_absolute() {
            link
        } else {
            std::path::absolute(source.parent().unwrap_or(Path::new(".")))?.join(link)
        };
        return std::os::unix::fs::symlink(link, target);
    }
    if source.is_dir() {
        fs::create_dir_all(target)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            copy_tree(&entry.path(), &target.join(entry.file_name()))?;
        }
    } else {
        fs::copy(source, target)?;
    }
    Ok(())
}
pub fn migrate_engine_state_to_brand_dir(
    legacy: &Path,
    target: &Path,
) -> io::Result<BrandDirMigrationResult> {
    let mut result = BrandDirMigrationResult {
        migrated: false,
        copied: Vec::new(),
        from: legacy.to_owned(),
        to: target.to_owned(),
    };
    if !legacy.exists()
        || target.join(MIGRATION_MARKER).exists()
        || target.join("settings.json").exists()
    {
        return Ok(result);
    }
    let Ok(entries) = fs::read_dir(legacy) else {
        return Ok(result);
    };
    fs::create_dir_all(target)?;
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if matches!(name.as_str(), "cache" | "logs" | "omo-local-update")
            || name.ends_with(".log")
            || is_desktop_reserved(&name)
            || target.join(&name).exists()
        {
            continue;
        }
        match copy_tree(&entry.path(), &target.join(&name)) {
            Ok(()) => result.copied.push(name),
            Err(error) => eprintln!("Brand migration could not copy entry: {error}"),
        }
    }
    if fs::write(
        target.join(MIGRATION_MARKER),
        format!("{}\n", legacy.display()),
    )
    .is_err()
    {
        return Ok(result);
    }
    result.migrated = true;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retains_copied_entries_when_marker_write_fails() {
        // Given: copying an entry creates a directory at the marker path.
        let root = tempfile::tempdir().expect("temporary migration root");
        let legacy = root.path().join("legacy");
        let target = root.path().join("brand");
        fs::create_dir_all(legacy.join(MIGRATION_MARKER)).expect("marker directory");
        fs::write(legacy.join("auth.json"), b"{}").expect("legacy auth");

        // When: the copy succeeds but writing the marker fails, even as root.
        let mut result =
            migrate_engine_state_to_brand_dir(&legacy, &target).expect("nonfatal marker failure");

        // Then: copied state is retained without reporting a completed migration.
        result.copied.sort();
        assert!(!result.migrated);
        assert_eq!(result.copied, vec![MIGRATION_MARKER, "auth.json"]);
        assert_eq!(result.from, legacy);
        assert_eq!(result.to, target);
        assert_eq!(
            fs::read(target.join("auth.json")).expect("copied auth"),
            b"{}"
        );
    }

    #[test]
    fn completes_migration_when_marker_write_succeeds() {
        // Given: legacy state and an empty destination.
        let root = tempfile::tempdir().expect("temporary migration root");
        let legacy = root.path().join("legacy");
        let target = root.path().join("brand");
        fs::create_dir(&legacy).expect("legacy directory");
        fs::write(legacy.join("auth.json"), b"{}").expect("legacy auth");

        // When: the state and marker can both be written.
        let result = migrate_engine_state_to_brand_dir(&legacy, &target).expect("migration");

        // Then: the completed migration has a source-bearing marker.
        assert!(result.migrated);
        assert_eq!(result.copied, vec!["auth.json"]);
        assert_eq!(
            fs::read_to_string(target.join(MIGRATION_MARKER)).expect("migration marker"),
            format!("{}\n", legacy.display())
        );
    }
}
