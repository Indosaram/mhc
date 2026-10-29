use std::path::{Path, PathBuf};
use utils::contains_path::contains_path;
use super::types::{PluginInstallation, PluginScope};

fn expand_tilde(path: &str) -> PathBuf {
    if path == "~" { return dirs::home_dir().unwrap_or_default(); }
    if let Some(rest) = path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        return dirs::home_dir().unwrap_or_default().join(rest);
    }
    PathBuf::from(path)
}

pub fn should_load_plugin_for_cwd(installation: &PluginInstallation, cwd: &Path) -> bool {
    match installation.scope {
        PluginScope::User | PluginScope::Managed => true,
        PluginScope::Project | PluginScope::Local => installation.project_path.as_deref().filter(|path| !path.is_empty())
            .is_some_and(|path| contains_path(expand_tilde(path), cwd)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn global_scopes_load_and_project_scopes_require_containment() {
        let root = tempfile::tempdir().expect("fixture directory");
        for scope in [PluginScope::User, PluginScope::Managed] {
            assert!(should_load_plugin_for_cwd(&PluginInstallation { scope, ..Default::default() }, root.path()));
        }
        for scope in [PluginScope::Project, PluginScope::Local] {
            assert!(!should_load_plugin_for_cwd(&PluginInstallation { scope, ..Default::default() }, root.path()));
            let installation = PluginInstallation { scope, project_path: Some(root.path().join("project").to_string_lossy().into_owned()), ..Default::default() };
            for (cwd, expected) in [(root.path().join("project"), true), (root.path().join("project/packages/app"), true), (root.path().join("other"), false), (root.path().to_path_buf(), false)] {
                assert_eq!(should_load_plugin_for_cwd(&installation, &cwd), expected);
            }
        }
        let home = dirs::home_dir().expect("home directory");
        for path in ["~", "~/workspace/proj-a", "~\\workspace/proj-a"] {
            let installation = PluginInstallation { scope: PluginScope::Project, project_path: Some(path.into()), ..Default::default() };
            let cwd = if path == "~" { home.clone() } else { home.join("workspace/proj-a") };
            assert!(should_load_plugin_for_cwd(&installation, &cwd));
            if path != "~" { assert!(!should_load_plugin_for_cwd(&installation, &home.join("workspace/proj-b"))); }
        }
    }
}
