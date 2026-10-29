use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};

#[derive(Deserialize)]
struct Entry {
    path: String,
    sha256: String,
    size: u64,
    mode: String,
}

#[derive(Deserialize)]
struct Collection {
    files: Vec<Entry>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    schema_version: u32,
    executables: Vec<Entry>,
    licenses: Vec<Entry>,
    skills: Collection,
}

/// Executables every package carries.
const REQUIRED_EXECUTABLES: [&str; 2] = ["mhc", "mhc-launcher"];
/// Sibling executables a package may carry beside `mhc` (tools/package-native.mjs `SIBLINGS`).
const SIBLING_EXECUTABLES: [&str; 4] = ["mhc-lsp", "senpi-desktop-engine", "mhc-git-bash", "mhc-ast-grep"];

pub fn relative_path(value: &str) -> Result<PathBuf, String> {
    if value.is_empty() || value.contains(['\\', ':', '\0']) {
        return Err(format!("invalid package path: {value:?}"));
    }
    for segment in value.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(format!("invalid package path component: {value:?}"));
        }
        #[cfg(windows)]
        {
            let stem = segment.split('.').next().unwrap_or_default().to_ascii_uppercase();
            let numbered_device = stem.strip_prefix("COM").or_else(|| stem.strip_prefix("LPT"))
                .is_some_and(|suffix| suffix.len() == 1 && matches!(suffix.as_bytes()[0], b'1'..=b'9'));
            if segment.ends_with([' ', '.'])
                || segment.contains(['<', '>', '"', '|', '?', '*'])
                || segment.chars().any(char::is_control)
                || matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                || numbered_device
            {
                return Err(format!("invalid Windows package path: {value:?}"));
            }
        }
    }
    let path = Path::new(value);
    if !path.components().all(|part| matches!(part, Component::Normal(_))) {
        return Err(format!("package path escapes its root: {value:?}"));
    }
    Ok(path.to_path_buf())
}

pub fn sha256(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let count = file.read(&mut buffer).map_err(|e| format!("hash {}: {e}", path.display()))?;
        if count == 0 { break; }
        hash.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn regular_files(root: &Path, directory: &Path, files: &mut BTreeSet<PathBuf>) -> Result<(), String> {
    for entry in fs::read_dir(directory).map_err(|e| format!("inspect {}: {e}", directory.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let kind = entry.file_type().map_err(|e| e.to_string())?;
        if kind.is_dir() {
            regular_files(root, &entry.path(), files)?;
        } else if kind.is_file() {
            files.insert(entry.path().strip_prefix(root).map_err(|e| e.to_string())?.to_path_buf());
        } else {
            return Err(format!("package contains non-regular entry: {}", entry.path().display()));
        }
    }
    Ok(())
}

pub fn validate(root: &Path) -> Result<(), String> {
    let manifest_path = root.join("manifest.json");
    let metadata = fs::symlink_metadata(&manifest_path).map_err(|e| format!("manifest: {e}"))?;
    if !metadata.is_file() || metadata.len() > 32 * 1024 * 1024 {
        return Err("manifest is not a bounded regular file".into());
    }
    let manifest: Manifest = serde_json::from_slice(&fs::read(&manifest_path).map_err(|e| e.to_string())?)
        .map_err(|e| format!("invalid package manifest: {e}"))?;
    if manifest.schema_version != 1 || manifest.skills.files.is_empty() || manifest.licenses.is_empty() {
        return Err("unsupported or incomplete package manifest".into());
    }
    let suffix = std::env::consts::EXE_SUFFIX;
    for name in REQUIRED_EXECUTABLES {
        let expected = format!("{name}{suffix}");
        if !manifest.executables.iter().any(|entry| entry.path == expected) {
            return Err(format!("package missing executable {expected}"));
        }
    }
    for entry in &manifest.executables {
        let known = REQUIRED_EXECUTABLES.iter().chain(&SIBLING_EXECUTABLES)
            .any(|name| entry.path == format!("{name}{suffix}"));
        if !known {
            return Err(format!("package lists unknown executable {}", entry.path));
        }
    }
    let mut expected = BTreeSet::from([PathBuf::from("manifest.json")]);
    let canonical_root = fs::canonicalize(root).map_err(|e| e.to_string())?;
    let entries = manifest.executables.iter().chain(&manifest.licenses).chain(&manifest.skills.files);
    for entry in entries {
        let relative = relative_path(&entry.path)?;
        if !expected.insert(relative.clone()) {
            return Err(format!("duplicate manifest path: {}", entry.path));
        }
        let path = root.join(&relative);
        let metadata = fs::symlink_metadata(&path).map_err(|e| format!("{}: {e}", entry.path))?;
        if !metadata.is_file() || metadata.len() != entry.size {
            return Err(format!("package file size/type mismatch: {}", entry.path));
        }
        let mode = u32::from_str_radix(&entry.mode, 8).map_err(|e| format!("invalid file mode: {e}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o777 != mode & 0o777 {
                return Err(format!("package file mode mismatch: {}", entry.path));
            }
        }
        #[cfg(not(unix))]
        let _ = mode;
        let canonical = fs::canonicalize(&path).map_err(|e| e.to_string())?;
        if !canonical.starts_with(&canonical_root) || sha256(&path)? != entry.sha256.to_ascii_lowercase() {
            return Err(format!("package file integrity mismatch: {}", entry.path));
        }
    }
    let mut actual = BTreeSet::new();
    regular_files(root, root, &mut actual)?;
    if actual != expected {
        return Err("package contains files missing from its manifest".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
        let write = |name: &str| -> Result<serde_json::Value, Box<dyn std::error::Error>> {
            let path = root.join(name);
            fs::create_dir_all(path.parent().ok_or("missing parent")?)?;
            fs::write(&path, b"packaged content")?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o644))?;
            }
            Ok(serde_json::json!({
                "path": name, "sha256": sha256(&path)?, "size": 16, "mode": "644"
            }))
        };
        let executables = REQUIRED_EXECUTABLES.iter().chain(&SIBLING_EXECUTABLES)
            .map(|name| write(&format!("{name}{}", std::env::consts::EXE_SUFFIX)))
            .collect::<Result<Vec<_>, _>>()?;
        let manifest = serde_json::json!({
            "schemaVersion": 1,
            "executables": executables,
            "licenses": [write("licenses/LICENSE")?],
            "skills": { "files": [write("skills/browser/SKILL.md")?] }
        });
        fs::write(root.join("manifest.json"), serde_json::to_vec(&manifest)?)?;
        Ok(())
    }

    #[test]
    fn validates_complete_package_and_detects_same_length_tampering() -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        fixture(root.path())?;
        validate(root.path())?;
        fs::write(root.path().join("skills/browser/SKILL.md"), b"modified content")?;
        assert!(validate(root.path()).is_err());
        Ok(())
    }

    #[test]
    fn requires_payload_and_launcher_but_not_siblings() -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        fixture(root.path())?;
        let manifest_path = root.path().join("manifest.json");
        let mut manifest: serde_json::Value = serde_json::from_slice(&fs::read(&manifest_path)?)?;
        let executables = manifest["executables"].as_array_mut().ok_or("executables")?;
        let sibling = format!("mhc-ast-grep{}", std::env::consts::EXE_SUFFIX);
        executables.retain(|entry| entry["path"] != sibling.as_str());
        fs::write(&manifest_path, serde_json::to_vec(&manifest)?)?;
        fs::remove_file(root.path().join(&sibling))?;
        validate(root.path())?;

        let launcher = format!("mhc-launcher{}", std::env::consts::EXE_SUFFIX);
        let executables = manifest["executables"].as_array_mut().ok_or("executables")?;
        executables.retain(|entry| entry["path"] != launcher.as_str());
        fs::write(&manifest_path, serde_json::to_vec(&manifest)?)?;
        fs::remove_file(root.path().join(&launcher))?;
        assert!(validate(root.path()).is_err());
        Ok(())
    }

    #[test]
    fn rejects_unknown_executables() -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        fixture(root.path())?;
        let manifest_path = root.path().join("manifest.json");
        let mut manifest: serde_json::Value = serde_json::from_slice(&fs::read(&manifest_path)?)?;
        let name = format!("lsp-daemon{}", std::env::consts::EXE_SUFFIX);
        fs::write(root.path().join(&name), b"packaged content")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(root.path().join(&name), fs::Permissions::from_mode(0o644))?;
        }
        let digest = sha256(&root.path().join(&name))?;
        let entry = serde_json::json!({ "path": name, "sha256": digest, "size": 16, "mode": "644" });
        manifest["executables"].as_array_mut().ok_or("executables")?.push(entry);
        fs::write(&manifest_path, serde_json::to_vec(&manifest)?)?;
        assert!(validate(root.path()).is_err());
        Ok(())
    }

    #[test]
    fn rejects_untracked_files_before_replacement() -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        fixture(root.path())?;
        fs::write(root.path().join("user-notes.txt"), b"preserve me")?;
        assert!(validate(root.path()).is_err());
        assert_eq!(fs::read(root.path().join("user-notes.txt"))?, b"preserve me");
        Ok(())
    }

    #[test]
    fn rejects_cross_platform_escape_paths() {
        for value in ["", "../mhc", "/mhc", "C:/mhc", "skills\\..\\mhc", "skills/../../mhc", "skills/./mhc", "skills//mhc"] {
            assert!(relative_path(value).is_err(), "{value:?}");
        }
        assert_eq!(relative_path("skills/browser/SKILL.md").expect("relative path"), PathBuf::from("skills/browser/SKILL.md"));
    }

    #[cfg(windows)]
    #[test]
    fn rejects_device_names_and_normalization_aliases() {
        for value in ["NUL", "skills/CON.txt", "LPT1.log", "skills/file.", "skills/file ", "skills/a?b"] {
            assert!(relative_path(value).is_err(), "{value:?}");
        }
        assert!(relative_path("skills/console.md").is_ok());
    }
}
