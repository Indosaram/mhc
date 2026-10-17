use std::{fs, io, path::Path};
fn regenerable(name: &str) -> bool { matches!(name, "cache" | "logs" | "tmp" | "migrations-state.json") || name.ends_with(".log") }
fn copy_entry(source: &Path, target: &Path) -> io::Result<()> {
    if source.is_dir() {
        fs::create_dir_all(target)?;
        for entry in fs::read_dir(source)? { let entry = entry?; copy_entry(&entry.path(), &target.join(entry.file_name()))?; }
    } else { fs::copy(source, target)?; }
    Ok(())
}
fn same_location(old: &Path, new: &Path) -> bool { new.exists() && fs::canonicalize(old).ok() == fs::canonicalize(new).ok() }
/// senpi `copyLegacyDir`: an official `.pi` dir belongs to a pi install that keeps running, so its
/// non-regenerable entries are only ever copied; the source stays untouched.
pub fn migrate_path_preserving_existing(old: &Path, new: &Path, label: &str) -> io::Result<()> {
    if !old.exists() || same_location(old, new) { return Ok(()); }
    let Ok(entries) = fs::read_dir(old) else { return Ok(()); }; let mut copied = false;
    for entry in entries { let entry = entry?; let name = entry.file_name(); let name = name.to_string_lossy(); if regenerable(&name) { continue; } let target = new.join(entry.file_name()); if target.exists() { continue; } fs::create_dir_all(new)?; match copy_entry(&entry.path(), &target) { Ok(()) => copied = true, Err(error) => eprintln!("Could not copy {} → {}: {error}", entry.path().display(), target.display()) } }
    if copied { crate::cli::stdout_guard::write_line(&format!("Copied {label} {} → {}\n", old.display(), new.display())); } Ok(())
}
/// senpi `moveLegacyDir`: a `.pi` leftover nested in our own config dir is moved.
fn move_legacy_dir(old: &Path, new: &Path, label: &str) -> io::Result<()> {
    if !old.exists() || same_location(old, new) { return Ok(()); }
    if !new.exists() { if let Some(parent) = new.parent() { fs::create_dir_all(parent)?; } fs::rename(old, new)?; crate::cli::stdout_guard::write_line(&format!("Migrated {label} {} → {}\n", old.display(), new.display())); return Ok(()); }
    let mut moved = false;
    for entry in fs::read_dir(old)? { let entry = entry?; let target = new.join(entry.file_name()); if target.exists() { continue; } fs::rename(entry.path(), target)?; moved = true; }
    if moved { crate::cli::stdout_guard::write_line(&format!("Migrated missing {label} entries {} → {}\n", old.display(), new.display())); } Ok(())
}
pub fn migrate_legacy_senpi_dirs(cwd: &Path, home: &Path, agent: &Path) -> io::Result<()> {
    let config_dir = maho_core::config::config_dir_name();
    if config_dir == ".pi" { return Ok(()); }
    let config = home.join(&config_dir); let project = cwd.join(&config_dir);
    // (from, to, label, move): official `.pi` dirs are copied, `.pi` leftovers nested in our config dir are moved.
    let mut dirs = vec![(cwd.join(".pi"), project.clone(), "project config directory", false), (project.join(".pi"), project, "nested project config directory", true)];
    if agent.starts_with(&config) { let mut globals = vec![(home.join(".pi/agent"), agent.to_owned(), "global agent directory", false), (config.join(".pi/agent"), agent.to_owned(), "nested global agent directory", true), (home.join(".pi/mom"), config.join("mom"), "global mom directory", false), (config.join(".pi/mom"), config.join("mom"), "nested global mom directory", true)]; globals.append(&mut dirs); dirs = globals; }
    for (old, new, label, moves) in dirs {
        let result = if moves { move_legacy_dir(&old, &new, label) } else { migrate_path_preserving_existing(&old, &new, label) };
        if let Err(error) = result { eprintln!("Skipped the {label} {}: {error}", old.display()); }
    }
    Ok(())
}
