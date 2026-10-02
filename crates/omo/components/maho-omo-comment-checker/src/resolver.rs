use std::{collections::BTreeMap,path::{Path,PathBuf}};
use std::os::unix::fs::PermissionsExt;
use crate::constants::COMMENT_CHECKER_ENV_KEY;
pub fn resolve_senpi_comment_checker_binary(env:&BTreeMap<String,String>,package_binary:Option<&Path>)->Option<PathBuf> {
    resolve_with_options(env,Path::exists,||package_binary.map(Path::to_owned),|name|std::env::split_paths(env.get("PATH")?).filter(|p|!p.as_os_str().is_empty()).map(|p|p.join(name)).find(|p|std::fs::metadata(p).is_ok_and(|m|m.permissions().mode()&0o111!=0)),"linux")
}
pub fn resolve_with_options(env:&BTreeMap<String,String>,exists:impl Fn(&Path)->bool,package_binary:impl Fn()->Option<PathBuf>,path_lookup:impl Fn(&str)->Option<PathBuf>,platform:&str)->Option<PathBuf> {
    if let Some(path)=env.get(COMMENT_CHECKER_ENV_KEY).map(|v|PathBuf::from(v.trim())).filter(|p|p.is_absolute()&&exists(p)) {return Some(path);}
    if let Some(path)=package_binary().filter(|p|exists(p)) {return Some(path);}
    path_lookup(if platform=="win32" {"comment-checker.exe"}else{"comment-checker"})
}
