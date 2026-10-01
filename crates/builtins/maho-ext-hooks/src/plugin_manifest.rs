use std::collections::BTreeMap;
use std::path::{Path,PathBuf};
use crate::types::{HookDiscoveryTiming,HookSourceMetadata,HookSourceScope};

pub const MANIFEST_PATH:&str=".codex-plugin/plugin.json";
pub const DEFAULT_HOOK_PATH:&str="hooks/hooks.json";
pub struct LoadPluginHookManifestOptions {
    pub plugin_root:PathBuf,
    pub display_order:usize,
    pub discovered_at:Option<HookDiscoveryTiming>,
    pub data_root:Option<PathBuf>,
    pub include_default_hooks:bool,
}

pub fn build_plugin_env(plugin_root:&Path,data_root:Option<&Path>)->std::io::Result<BTreeMap<String,String>> {
    let data=match data_root {Some(path)=>std::path::absolute(path)?,None=>plugin_root.join(".plugin-data")};
    Ok([("PLUGIN_ROOT",plugin_root),("PLUGIN_DATA",data.as_path()),("CLAUDE_PLUGIN_ROOT",plugin_root),("CLAUDE_PLUGIN_DATA",data.as_path())].into_iter().map(|(key,path)|(key.to_owned(),path.to_string_lossy().into_owned())).collect())
}

pub fn source_for_path(options:&LoadPluginHookManifestOptions,env:&BTreeMap<String,String>,source_path:&Path)->std::io::Result<HookSourceMetadata> {
    let root=std::path::absolute(&options.plugin_root)?;
    Ok(HookSourceMetadata {scope:HookSourceScope::Plugin,source_path:source_path.to_string_lossy().into_owned(),display_order:options.display_order,discovered_at:options.discovered_at.clone().unwrap_or(HookDiscoveryTiming::PreSession),plugin_root:Some(root.to_string_lossy().into_owned()),manifest_path:Some(root.join(MANIFEST_PATH).to_string_lossy().into_owned()),plugin_env:Some(env.clone())})
}

pub fn normalize_path(path:&Path)->PathBuf {
    let mut normalized=PathBuf::new();
    for component in path.components() {match component {std::path::Component::CurDir=>{},std::path::Component::ParentDir=>{normalized.pop();},component=>normalized.push(component.as_os_str())}}
    normalized
}

pub fn is_contained(root:&Path,target:&Path)->bool {target==root || target.strip_prefix(root).is_ok_and(|rel|!rel.to_string_lossy().starts_with(".."))}

pub fn resolve_contained_path(plugin_root:&Path,input:&str)->std::io::Result<PathBuf> {
    let root=normalize_path(&std::path::absolute(plugin_root)?);
    let input=input.replace('\\',"/");let input=Path::new(input.strip_prefix("./").unwrap_or(&input));
    let path=normalize_path(&if input.is_absolute() {input.to_owned()} else {root.join(input)});
    if !is_contained(&root,&path) || path.is_file() && !is_contained(&std::fs::canonicalize(&root)?,&std::fs::canonicalize(&path)?) {
        return Err(std::io::Error::other(format!("Plugin hook path is outside plugin root: {}",input.display())));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn lexical_escape_rejected()->std::io::Result<()> {let root=tempfile::tempdir()?;assert!(resolve_contained_path(root.path(),"../escape.json").is_err());assert_eq!(resolve_contained_path(root.path(),"./hooks/hooks.json")?,root.path().join("hooks/hooks.json"));Ok(())}
    #[test] fn plugin_environment_is_symmetric()->std::io::Result<()> {let root=tempfile::tempdir()?;let env=build_plugin_env(root.path(),None)?;assert_eq!(env["PLUGIN_ROOT"],env["CLAUDE_PLUGIN_ROOT"]);assert_eq!(env["PLUGIN_DATA"],env["CLAUDE_PLUGIN_DATA"]);Ok(())}
}
