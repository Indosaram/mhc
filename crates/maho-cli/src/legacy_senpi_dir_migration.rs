use std::{fs, io, path::Path};
fn regenerable(name: &str) -> bool { matches!(name, "cache" | "logs" | "tmp" | "migrations-state.json") || name.ends_with(".log") }
fn copy_entry(source: &Path, target: &Path) -> io::Result<()> {
    if source.is_dir() {
        fs::create_dir_all(target)?;
        for entry in fs::read_dir(source)? { let entry = entry?; copy_entry(&entry.path(), &target.join(entry.file_name()))?; }
    } else { fs::copy(source, target)?; }
    Ok(())
}
pub fn migrate_path_preserving_existing(old: &Path, new: &Path, label: &str) -> io::Result<()> {
    if !old.exists() || (new.exists() && fs::canonicalize(old).ok() == fs::canonicalize(new).ok()) { return Ok(()); }
    if !new.exists() { if let Some(parent) = new.parent() { fs::create_dir_all(parent)?; } fs::rename(old, new)?; crate::cli::stdout_guard::write_line(&format!("Migrated {label} {} → {}\n", old.display(), new.display())); return Ok(()); }
    let Ok(entries) = fs::read_dir(old) else { return Ok(()); }; let mut copied = false;
    for entry in entries { let entry = entry?; let name = entry.file_name(); let name = name.to_string_lossy(); if regenerable(&name) { continue; } let target = new.join(entry.file_name()); if target.exists() { continue; } match copy_entry(&entry.path(), &target) { Ok(()) => copied = true, Err(error) => eprintln!("Legacy migration could not copy entry: {error}") } }
    if copied { crate::cli::stdout_guard::write_line(&format!("Migrated missing {label} entries {} → {}\n", old.display(), new.display())); } Ok(())
}
pub fn migrate_legacy_senpi_dirs(cwd: &Path, home: &Path, agent: &Path) -> io::Result<()> {
    let config_dir = maho_core::config::config_dir_name();
    if config_dir == ".pi" { return Ok(()); }
    let config = home.join(&config_dir); let project = cwd.join(&config_dir);
    let mut moves = vec![(cwd.join(".pi"), project.clone(), "project config directory"), (project.join(".pi"), project, "nested project config directory")];
    if agent.starts_with(&config) { let mut globals = vec![(home.join(".pi/agent"), agent.to_owned(), "global agent directory"), (config.join(".pi/agent"), agent.to_owned(), "nested global agent directory"), (home.join(".pi/mom"), config.join("mom"), "global mom directory"), (config.join(".pi/mom"), config.join("mom"), "nested global mom directory")]; globals.append(&mut moves); moves = globals; }
    for (old, new, label) in moves { if let Err(error) = migrate_path_preserving_existing(&old, &new, label) { eprintln!("Migration unavailable: {error}"); } } Ok(())
}
