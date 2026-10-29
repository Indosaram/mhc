use std::{collections::BTreeMap,path::{Path,PathBuf}};
use maho_omo_agent_home::resolve_agent_home;
pub const PLAN9_FILE_SYSTEM_TYPE:i64=0x0102_1997;
pub const OMO_CONFIG_FILE_FILTER_GLOBS:[&str;2]=["/omo.jsonc","/omo.json"];
pub const OMO_CONFIG_DIRECTORY_FILTER_GLOBS:[&str;3]=["/.omo","/.omo/omo.jsonc","/.omo/omo.json"];
pub const USER_OMO_CONFIG_DIRECTORY_FILTER_GLOBS:[&str;3]=["/.omo","/.omo/omo.jsonc","/.omo/omo.json"];
#[derive(Clone,Debug,PartialEq,Eq)]
pub struct OmoConfigWatchTarget { pub path:PathBuf,pub kind:&'static str,pub filter_globs:Vec<String> }
#[derive(Clone,Debug,PartialEq,Eq)]
pub struct OmoConfigWatchTargetResolution { pub targets:Vec<OmoConfigWatchTarget>,pub user_config_creation_watched:bool,pub user_config_creation_discovery:&'static str }
fn target(path:PathBuf,globs:&[&str])->OmoConfigWatchTarget { OmoConfigWatchTarget{path,kind:"dir",filter_globs:globs.iter().map(|s|s.to_string()).collect()} }
fn contains(parent:&Path,child:&Path)->bool { child.strip_prefix(parent).is_ok() }
fn default_filesystem_type(path:&Path)->Option<i64> {
    #[cfg(target_os="linux")]
    { rustix::fs::statfs(path).ok().map(|stat|stat.f_type) }
    #[cfg(not(target_os="linux"))]
    { let _=path; None }
}
fn restricted(target:&OmoConfigWatchTarget,protected:&[PathBuf])->bool {
    protected.iter().any(|path| {
        if contains(path,&target.path) { return true; }
        if !contains(&target.path,path) { return false; }
        target.filter_globs.is_empty() || !target.filter_globs.iter().all(|glob| {
            let Some(relative)=glob.strip_prefix('/') else { return false; };
            let glob_path=target.path.join(relative);
            !contains(&glob_path,path) && !contains(path,&glob_path)
        })
    })
}
fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => { normalized.pop(); },
            std::path::Component::CurDir => {},
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}
pub fn resolve_omo_config_watch_target_resolution(cwd:&Path,home:&Path,env:&BTreeMap<String,String>)->OmoConfigWatchTargetResolution {
    let cwd_path=normalize_path(cwd);let cwd=cwd_path.as_path();
    let home_path=normalize_path(home);let home=home_path.as_path();
    let mut loader_env=env.clone();
    loader_env.insert("HOME".into(),home.to_string_lossy().into_owned());
    if cfg!(windows) { loader_env.insert("USERPROFILE".into(),home.to_string_lossy().into_owned()); }
    let configured=omo_config_core::resolve_omo_config_paths(&omo_config_core::ResolveOmoConfigPathsOptions{cwd:cwd.to_string_lossy().into_owned(),env:Some(loader_env),file_system:None,platform:if cfg!(windows) { Some("win32".into()) } else { Some("linux".into()) }});
    let mut project_directories:std::collections::BTreeSet<PathBuf>=configured.into_iter().filter(|p|p.scope=="project").filter_map(|p|PathBuf::from(p.path).parent().map(Path::to_owned)).collect();
    project_directories.extend(omo_config_core::find_project_config_paths_farthest_first(&cwd.to_string_lossy(),&home.to_string_lossy(),&omo_config_core::StdReadFileSystem,None).into_iter().filter_map(|p|PathBuf::from(p).parent().map(Path::to_owned)));
    let user=home.join(".omo"); let mut targets=Vec::new();
    if user.is_dir() { if default_filesystem_type(&user)!=Some(PLAN9_FILE_SYSTEM_TYPE) { targets.push(target(user.clone(),&OMO_CONFIG_FILE_FILTER_GLOBS)); } }
    else if let Some(parent)=user.parent().filter(|p|p.is_dir()) && default_filesystem_type(parent)!=Some(PLAN9_FILE_SYSTEM_TYPE) { targets.push(target(parent.to_owned(),&USER_OMO_CONFIG_DIRECTORY_FILTER_GLOBS)); }
    let mut ancestors=Vec::new();
    for ancestor in cwd.ancestors().take(128) { ancestors.push(ancestor); if contains(home,cwd) && ancestor==home { break; } }
    if default_filesystem_type(cwd)!=Some(PLAN9_FILE_SYSTEM_TYPE) {
    for ancestor in &ancestors { let dir=ancestor.join(".omo"); if project_directories.contains(&dir) || std::fs::symlink_metadata(&dir).is_ok_and(|m|m.is_dir() && !m.file_type().is_symlink()) { targets.push(target(dir,&OMO_CONFIG_FILE_FILTER_GLOBS)); } }
    for ancestor in ancestors { targets.push(target(ancestor.to_owned(),&OMO_CONFIG_DIRECTORY_FILTER_GLOBS)); }
    }
    let agent=resolve_agent_home(env,home,cwd,Path::exists);
    let protected=[agent.join("auth.json"),agent.join("sessions"),agent.join("logs")];
    targets.retain(|t|!restricted(t,&protected));
    let user_config_creation_watched=targets.iter().any(|t|t.path==user||Some(t.path.as_path())==user.parent());
    OmoConfigWatchTargetResolution{targets,user_config_creation_watched,user_config_creation_discovery:if user_config_creation_watched {"watched"}else{"reload_required"}}
}
