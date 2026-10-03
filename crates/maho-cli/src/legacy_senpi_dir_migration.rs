use std::{fs, io, path::Path};
pub fn migrate_path_preserving_existing(old: &Path, new: &Path, label: &str) -> io::Result<()> {
    if !old.exists() || (new.exists() && fs::canonicalize(old).ok() == fs::canonicalize(new).ok()) { return Ok(()); }
    if !new.exists() { if let Some(parent) = new.parent() { fs::create_dir_all(parent)?; } fs::rename(old, new)?; println!("Migrated {label} {} → {}", old.display(), new.display()); return Ok(()); }
    let Ok(entries) = fs::read_dir(old) else { return Ok(()); }; let mut moved = false;
    for entry in entries { let entry = entry?; let target = new.join(entry.file_name()); if target.exists() { continue; } match fs::rename(entry.path(), target) { Ok(()) => moved = true, Err(error) => eprintln!("Legacy migration could not move entry: {error}") } }
    if moved { println!("Migrated missing {label} entries {} → {}", old.display(), new.display()); } Ok(())
}
pub fn migrate_legacy_senpi_dirs(cwd: &Path, home: &Path, agent: &Path) -> io::Result<()> {
    let config = home.join(".maho"); let project = cwd.join(".maho");
    let mut moves = vec![(cwd.join(".pi"), project.clone(), "project config directory"), (project.join(".pi"), project, "nested project config directory")];
    if agent.starts_with(&config) { let mut globals = vec![(home.join(".pi/agent"), agent.to_owned(), "global agent directory"), (config.join(".pi/agent"), agent.to_owned(), "nested global agent directory"), (home.join(".pi/mom"), config.join("mom"), "global mom directory"), (config.join(".pi/mom"), config.join("mom"), "nested global mom directory")]; globals.append(&mut moves); moves = globals; }
    for (old, new, label) in moves { if let Err(error) = migrate_path_preserving_existing(&old, &new, label) { eprintln!("Migration unavailable: {error}"); } } Ok(())
}
