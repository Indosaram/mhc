use std::path::{Component, Path, PathBuf};
use maho_core::generated_shim_banner::has_generated_shim_banner;

fn resolve(path: &Path) -> PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut resolved = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::ParentDir => { resolved.pop(); },
            Component::CurDir => {},
            Component::Normal(_) | Component::RootDir | Component::Prefix(_) => resolved.push(component.as_os_str()),
        }
    }
    resolved
}

pub fn exclude_generated_extension_shims(paths: &[PathBuf], agent_dir: &Path) -> Vec<PathBuf> {
    let extensions = resolve(&agent_dir.join("extensions"));
    paths.iter().filter(|path| {
        let inside = path.parent().is_some_and(|parent| resolve(parent) == extensions);
        !(inside && std::fs::read_to_string(path).is_ok_and(|content| has_generated_shim_banner(&content)))
    }).cloned().collect()
}
