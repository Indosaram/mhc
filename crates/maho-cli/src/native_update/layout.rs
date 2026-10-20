use std::fs;
use std::path::{Component, Path, PathBuf};

pub struct Layout {
    pub installation: PathBuf,
    #[cfg(windows)]
    pub executable: PathBuf,
}

fn resolved(path: &Path) -> Result<PathBuf, String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| error.to_string())?
            .join(path)
    };
    let mut normalized = PathBuf::new();
    for part in absolute.components() {
        match part {
            Component::Prefix(_) => {
                normalized.push(part.as_os_str());
            }
            Component::RootDir => {
                normalized.push(part.as_os_str());
                if let Ok(canon) = fs::canonicalize(&normalized) {
                    normalized = canon;
                }
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err("path escapes filesystem root".into());
                }
            }
            Component::Normal(name) => {
                normalized.push(name);
                match fs::symlink_metadata(&normalized) {
                    Ok(_) => {
                        normalized =
                            fs::canonicalize(&normalized).map_err(|error| error.to_string())?;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.to_string()),
                }
            }
        }
    }
    Ok(normalized)
}

pub fn reject_state_overlap(installation: &Path, state: &Path) -> Result<(), String> {
    let installation = resolved(installation)?;
    let state = resolved(state)?;
    #[cfg(windows)]
    {
        let inst_str = installation.to_string_lossy().to_lowercase();
        let state_str = state.to_string_lossy().to_lowercase();
        let inst_path = Path::new(&inst_str);
        let state_path = Path::new(&state_str);
        if inst_path.starts_with(state_path) || state_path.starts_with(inst_path) {
            return Err(
                "installation and user state overlap; refusing to replace user data".into(),
            );
        }
    }
    #[cfg(not(windows))]
    {
        if installation.starts_with(&state) || state.starts_with(&installation) {
            return Err(
                "installation and user state overlap; refusing to replace user data".into(),
            );
        }
    }
    Ok(())
}

impl Layout {
    pub fn current() -> Result<Self, String> {
        let executable =
            fs::canonicalize(std::env::current_exe().map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?;
        let installation = executable
            .parent()
            .ok_or("executable has no parent")?
            .to_path_buf();
        if !installation.join("manifest.json").is_file() {
            return Err("native update requires a complete release installation; install the packaged release first".into());
        }
        reject_state_overlap(
            &installation,
            Path::new(&maho_core::config::get_agent_dir()),
        )?;
        Ok(Self {
            installation,
            #[cfg(windows)]
            executable,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_missing_components_before_overlap_check() -> Result<(), Box<dyn std::error::Error>>
    {
        let root = tempfile::tempdir()?;
        let installation = root.path().join("package");
        fs::create_dir(&installation)?;
        assert!(
            reject_state_overlap(&installation, &root.path().join("missing/../package/state"))
                .is_err()
        );
        assert!(
            reject_state_overlap(&installation, &root.path().join("package-other/state")).is_ok()
        );
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn windows_overlap_handles_case_and_verbatim_missing_paths()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let installation = root.path().join("Package");
        fs::create_dir(&installation)?;
        let canonical = fs::canonicalize(&installation)?;
        assert!(reject_state_overlap(&installation, &canonical.join("future/state")).is_err());
        assert!(reject_state_overlap(&canonical, &installation.join("FUTURE/state")).is_err());
        let missing = root.path().join("NOT-CREATED");
        let lower = root.path().join("not-created/state");
        assert!(reject_state_overlap(&missing, &lower).is_err());
        assert!(reject_state_overlap(&missing, &root.path().join("not-created-other")).is_ok());
        Ok(())
    }

    #[test]
    fn rejects_nested_state_even_before_it_exists() -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let installation = root.path().join("package");
        fs::create_dir(&installation)?;
        assert!(
            reject_state_overlap(&installation, &installation.join("state/not-created")).is_err()
        );
        assert!(reject_state_overlap(&installation, root.path()).is_err());
        assert!(reject_state_overlap(&installation, &installation).is_err());
        assert!(reject_state_overlap(&installation, &root.path().join("state")).is_ok());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn resolves_existing_symlink_before_checking_missing_state()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let installation = root.path().join("package");
        fs::create_dir(&installation)?;
        let alias = root.path().join("alias");
        std::os::unix::fs::symlink(&installation, &alias)?;
        assert!(reject_state_overlap(&installation, &alias.join("future-state")).is_err());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn parent_component_is_resolved_after_symlink_target() -> Result<(), Box<dyn std::error::Error>>
    {
        let root = tempfile::tempdir()?;
        let installation = root.path().join("package");
        fs::create_dir_all(installation.join("nested"))?;
        let alias = root.path().join("alias");
        std::os::unix::fs::symlink(installation.join("nested"), &alias)?;
        assert!(reject_state_overlap(&installation, &alias.join("../future-state")).is_err());
        Ok(())
    }
}
