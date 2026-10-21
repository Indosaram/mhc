use super::js_installer::EnvironmentError;
use std::{
    io::Write,
    path::{Path, PathBuf},
};

pub fn remove_entry(path: &Path) -> Result<(), std::io::Error> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            std::fs::remove_dir_all(path)
        }
        Ok(_) => std::fs::remove_file(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<(), std::io::Error> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(bytes)
}
pub fn carry_npmrc_settings(root: &Path) -> Result<(), EnvironmentError> {
    let path = root.join(".npmrc");
    let text = std::fs::read_to_string(&path);
    remove_entry(&path)?;
    let Ok(text) = text else {
        return Ok(());
    };
    let lines = text
        .lines()
        .filter_map(|line| {
            let (key, value) = line.trim().split_once('=')?;
            let key = key.trim();
            let registry = key == "registry"
                || (key.starts_with('@')
                    && key.ends_with(":registry")
                    && !key[..key.len() - 9].contains([' ', '=', ':', '/']));
            if registry {
                let raw = value.trim();
                if raw.contains("${") {
                    return None;
                }
                let quote = if raw.len() >= 2
                    && (raw.starts_with('"') && raw.ends_with('"')
                        || raw.starts_with('\'') && raw.ends_with('\''))
                {
                    &raw[..1]
                } else {
                    ""
                };
                let raw = if quote.is_empty() {
                    raw
                } else {
                    &raw[1..raw.len() - 1]
                };
                let mut url = url::Url::parse(raw).ok()?;
                url.set_username("").ok()?;
                url.set_password(None).ok()?;
                url.set_query(None);
                url.set_fragment(None);
                Some(format!("{key}={quote}{url}{quote}"))
            } else if matches!(key, "strict-ssl" | "ca" | "cafile") {
                Some(line.trim().into())
            } else {
                None
            }
        })
        .collect::<Vec<String>>();
    if !lines.is_empty() {
        write_private(&path, format!("{}\n", lines.join("\n")).as_bytes())?;
    }
    Ok(())
}
pub fn seed_package_json(root: &Path) -> Result<(), EnvironmentError> {
    match write_private(&root.join("package.json"), b"{\"private\":true}\n") {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error.into()),
    }
}
pub fn recorded_file_specs(root: &Path) -> Vec<String> {
    let manifest = std::fs::read(root.join("package.json"))
        .ok()
        .and_then(|text| serde_json::from_slice::<serde_json::Value>(&text).ok());
    let mut result = Vec::new();
    if let Some(deps) = manifest
        .as_ref()
        .and_then(|value| value["dependencies"].as_object())
    {
        for spec in deps.values().filter_map(serde_json::Value::as_str) {
            if let Some(path) = spec.strip_prefix("file:") {
                result.push(path.into());
                if !Path::new(path).is_absolute() {
                    result.push(root.join(path).to_string_lossy().into_owned());
                }
            } else if PathBuf::from(spec).is_absolute() {
                result.push(spec.into());
            }
        }
    }
    result
}
