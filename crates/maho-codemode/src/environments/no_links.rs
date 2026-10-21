use super::js_installer::EnvironmentError;
use serde_json::Value;
use std::{
    collections::HashMap,
    path::{Component, Path, PathBuf},
};

pub fn normalize(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            other => result.push(other.as_os_str()),
        }
    }
    result
}
pub fn assert_no_links_below(root: &Path, path: &Path) -> Result<(), EnvironmentError> {
    let relative = path.strip_prefix(root).map_err(|_| {
        EnvironmentError::failed("refusing to write outside the managed environment")
    })?;
    let mut current = root.to_path_buf();
    for part in relative.components() {
        if !matches!(part, Component::Normal(_)) {
            return Err(EnvironmentError::failed(
                "refusing to write outside the managed environment",
            ));
        }
        current.push(part);
        match std::fs::symlink_metadata(&current) {
            Ok(info) if info.file_type().is_symlink() => {
                return Err(EnvironmentError::failed(format!(
                    "{} is a link; refusing to write through it",
                    current.strip_prefix(root).unwrap_or(&current).display()
                )));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}
pub fn recorded_install_sources(revision: &Path) -> HashMap<String, PathBuf> {
    let manifest = std::fs::read(revision.join("package.json"))
        .ok()
        .and_then(|text| serde_json::from_slice::<Value>(&text).ok());
    let mut sources = HashMap::new();
    if let Some(dependencies) = manifest
        .as_ref()
        .and_then(|value| value["dependencies"].as_object())
    {
        for (name, spec) in dependencies {
            let Some(spec) = spec.as_str() else {
                continue;
            };
            let path = spec.strip_prefix("file:").unwrap_or(spec);
            if !Path::new(path).is_absolute() && !path.starts_with("./") && !path.starts_with("../")
            {
                continue;
            }
            if let Ok(source) = std::fs::canonicalize(revision.join(path))
                && source.is_dir()
            {
                sources.insert(name.clone(), source);
            }
        }
    }
    sources
}
pub fn assert_link_stays_inside(
    path: &Path,
    tree: &Path,
    sources: &HashMap<String, PathBuf>,
) -> Result<(), EnvironmentError> {
    if !std::fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Ok(());
    }
    let target = std::fs::read_link(path)?;
    let resolved = normalize(&path.parent().unwrap_or(tree).join(&target));
    if resolved.starts_with(tree) {
        if target.is_relative() {
            return Ok(());
        }
        return Err(EnvironmentError::failed(
            "an absolute link into its revision cannot be copied",
        ));
    }
    let relative = path.strip_prefix(tree).map_err(EnvironmentError::failed)?;
    let parts = relative
        .components()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    if parts.first().is_some_and(|part| part == "node_modules")
        && let Some(first) = parts.get(1)
    {
        let scoped = first.starts_with('@');
        let name = if scoped {
            format!("{first}/{}", parts.get(2).map_or("", String::as_str))
        } else {
            first.clone()
        };
        if let Some(source) = sources.get(&name) {
            let expected = parts
                .iter()
                .skip(if scoped { 3 } else { 2 })
                .fold(source.clone(), |path, part| path.join(part));
            if std::fs::canonicalize(&resolved).is_ok_and(|actual| actual == expected) {
                return Ok(());
            }
        }
    }
    Err(EnvironmentError::failed(format!(
        "{} links outside its revision; refusing to build on it",
        relative.display()
    )))
}
pub fn assert_built_revision_links(revision: &Path) -> Result<(), EnvironmentError> {
    let sources = recorded_install_sources(revision);
    let mut directories = vec![revision.to_path_buf()];
    while let Some(directory) = directories.pop() {
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                assert_link_stays_inside(&entry.path(), revision, &sources)?;
            } else if kind.is_dir() {
                directories.push(entry.path());
            }
        }
    }
    Ok(())
}
