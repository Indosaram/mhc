use crate::types::{Rule, Ruleset};
use std::{fs::{self, OpenOptions}, io::{self, Write}, path::{Path, PathBuf}};

fn permissions_path(project: &Path) -> PathBuf { project.join(".maho/permissions-approved.jsonl") }
pub fn load_approved(project: &Path) -> io::Result<Ruleset> {
    let content = match fs::read_to_string(permissions_path(project)) {
        Ok(content) => content, Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()), Err(error) => return Err(error),
    };
    Ok(content.lines().filter(|line| !line.trim().is_empty()).filter_map(|line| serde_json::from_str(line).ok()).collect())
}
pub fn append_approved(project: &Path, rules: &[Rule]) -> io::Result<()> {
    if rules.is_empty() { return Ok(()); }
    fs::create_dir_all(project.join(".maho"))?;
    let mut file = OpenOptions::new().append(true).create(true).open(permissions_path(project))?;
    for rule in rules { serde_json::to_writer(&mut file,rule)?; file.write_all(b"\n")?; }
    Ok(())
}
pub fn clear_approved(project: &Path) -> io::Result<()> {
    match fs::remove_file(permissions_path(project)) { Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()), result => result }
}
pub fn compact_approved(project: &Path) -> io::Result<()> {
    let path=permissions_path(project);
    if !path.try_exists()? { return Ok(()); }
    let mut unique: Vec<(String,Rule)> = Vec::new();
    for rule in load_approved(project)? {
        let key=format!("{}:{}",rule.permission,rule.pattern);
        if let Some(entry)=unique.iter_mut().find(|entry| entry.0 == key) { entry.1=rule; }
        else { unique.push((key,rule)); }
    }
    let mut file=OpenOptions::new().write(true).truncate(true).open(path)?;
    for (_,rule) in unique { serde_json::to_writer(&mut file,&rule)?; file.write_all(b"\n")?; }
    Ok(())
}
