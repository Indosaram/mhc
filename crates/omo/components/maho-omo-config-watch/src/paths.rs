use std::{collections::BTreeMap,path::{Path,PathBuf}};
use maho_omo_agent_home::resolve_agent_home;
pub const OMO_CONFIG_FILE_FILTER_GLOBS:[&str;2]=["/omo.jsonc","/omo.json"];
pub const OMO_CONFIG_DIRECTORY_FILTER_GLOBS:[&str;3]=["/.omo","/.omo/omo.jsonc","/.omo/omo.json"];
pub const USER_OMO_CONFIG_DIRECTORY_FILTER_GLOBS:[&str;1]=["/maho"];
pub struct OmoConfigWatchTarget { pub path:PathBuf,pub filter_globs:Vec<String> }
pub struct OmoConfigWatchTargetResolution { pub targets:Vec<OmoConfigWatchTarget>,pub user_config_creation_watched:bool }
fn target(path:PathBuf,globs:&[&str])->OmoConfigWatchTarget { OmoConfigWatchTarget{path,filter_globs:globs.iter().map(|s|s.to_string()).collect()} }
fn contains(parent:&Path,child:&Path)->bool { child.strip_prefix(parent).is_ok() }
pub fn resolve_omo_config_watch_target_resolution(cwd:&Path,home:&Path,env:&BTreeMap<String,String>)->OmoConfigWatchTargetResolution {
    let mut loader_env=env.clone();loader_env.insert("HOME".into(),home.to_string_lossy().into_owned());
    let configured=omo_config_core::resolve_omo_config_paths(&omo_config_core::ResolveOmoConfigPathsOptions{cwd:cwd.to_string_lossy().into_owned(),env:Some(loader_env),file_system:None,platform:Some("linux".into())});
    let mut project_directories:std::collections::BTreeSet<PathBuf>=configured.into_iter().filter(|p|p.scope=="project").filter_map(|p|PathBuf::from(p.path).parent().map(Path::to_owned)).collect();
    project_directories.extend(omo_config_core::find_project_config_paths_farthest_first(&cwd.to_string_lossy(),&home.to_string_lossy(),&omo_config_core::StdReadFileSystem,None).into_iter().filter_map(|p|PathBuf::from(p).parent().map(Path::to_owned)));
    let user=home.join(".maho"); let mut targets=Vec::new();
    if user.is_dir() { targets.push(target(user.clone(),&OMO_CONFIG_FILE_FILTER_GLOBS)); }
    else if let Some(parent)=user.parent().filter(|p|p.is_dir()) { targets.push(target(parent.to_owned(),&USER_OMO_CONFIG_DIRECTORY_FILTER_GLOBS)); }
    let mut ancestors=Vec::new();
    for ancestor in cwd.ancestors().take(128) { ancestors.push(ancestor); if contains(home,cwd) && ancestor==home { break; } }
    for ancestor in &ancestors { let dir=ancestor.join(".omo"); if project_directories.contains(&dir) || std::fs::symlink_metadata(&dir).is_ok_and(|m|m.is_dir() && !m.file_type().is_symlink()) { targets.push(target(dir,&OMO_CONFIG_FILE_FILTER_GLOBS)); } }
    for ancestor in ancestors { targets.push(target(ancestor.to_owned(),&OMO_CONFIG_DIRECTORY_FILTER_GLOBS)); }
    let agent=resolve_agent_home(env,home,cwd,Path::exists);
    let protected=[agent.join("auth.json"),agent.join("sessions"),agent.join("logs")];
    targets.retain(|t|!protected.iter().any(|p|contains(&t.path,p)||contains(p,&t.path)));
    let user_config_creation_watched=targets.iter().any(|t|t.path==user||Some(t.path.as_path())==user.parent());
    OmoConfigWatchTargetResolution{targets,user_config_creation_watched}
}
