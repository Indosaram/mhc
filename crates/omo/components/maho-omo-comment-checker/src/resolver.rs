use std::{collections::BTreeMap,path::{Path,PathBuf}};
use std::os::unix::fs::PermissionsExt;
use crate::constants::COMMENT_CHECKER_ENV_KEY;
pub fn resolve_senpi_comment_checker_binary(env:&BTreeMap<String,String>,package_binary:Option<&Path>)->Option<PathBuf> {
    if let Some(path)=env.get(COMMENT_CHECKER_ENV_KEY).map(|v|PathBuf::from(v.trim())).filter(|p|p.is_absolute()&&p.exists()) { return Some(path); }
    if let Some(path)=package_binary.filter(|p|p.exists()) { return Some(path.to_owned()); }
    std::env::split_paths(env.get("PATH")?).filter(|p|!p.as_os_str().is_empty()).map(|p|p.join("comment-checker")).find(|p|std::fs::metadata(p).is_ok_and(|m|m.permissions().mode()&0o111!=0))
}
