use std::path::{Path, PathBuf};

pub fn project_lsp_config_paths(cwd:&Path)->Vec<PathBuf> {
    [".omo",".pi"].into_iter().map(|dir|cwd.join(dir).join("lsp-client.json")).collect()
}
fn first_existing_or_branded(home:&Path,file:&str,exists:impl Fn(&Path)->bool)->PathBuf {
    let branded=home.join(".omo").join(file);
    if exists(&branded) {return branded;}
    let legacy=home.join(".pi").join(file);
    if exists(&legacy) {legacy} else {branded}
}
pub fn user_lsp_config_path(home:&Path,exists:impl Fn(&Path)->bool)->PathBuf {
    first_existing_or_branded(home,"lsp-client.json",exists)
}
pub fn lsp_install_decisions_path(home:&Path,exists:impl Fn(&Path)->bool)->PathBuf {
    first_existing_or_branded(home,"lsp-install-decisions.json",exists)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn project_paths_prefer_project_brand() {assert_eq!(project_lsp_config_paths(Path::new("/project")),vec![PathBuf::from("/project/.omo/lsp-client.json"),PathBuf::from("/project/.pi/lsp-client.json")]);}
    #[test] fn user_paths_prefer_existing_brand() {let home=Path::new("/home/fixture");assert_eq!(user_lsp_config_path(home,|_|true),home.join(".omo").join("lsp-client.json"));assert_eq!(lsp_install_decisions_path(home,|_|true),home.join(".omo").join("lsp-install-decisions.json"));}
    #[test] fn user_paths_fall_back_to_existing_legacy() {let home=Path::new("/home/fixture");assert_eq!(user_lsp_config_path(home,|p|p.starts_with(home.join(".pi"))),home.join(".pi").join("lsp-client.json"));}
    #[test] fn both_brand_and_legacy_prefer_brand() {let home=Path::new("/home/fixture");assert_eq!(user_lsp_config_path(home,|p|p.starts_with(home.join(".omo")) || p.starts_with(home.join(".pi"))),home.join(".omo").join("lsp-client.json"));}
    #[test] fn absent_user_files_use_brand() {let home=Path::new("/home/fixture");assert_eq!(user_lsp_config_path(home,|_|false),home.join(".omo").join("lsp-client.json"));assert_eq!(lsp_install_decisions_path(home,|_|false),home.join(".omo").join("lsp-install-decisions.json"));}
}
