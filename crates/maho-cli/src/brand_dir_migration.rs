use std::{fs, io, path::{Path, PathBuf}};
pub const MIGRATION_MARKER: &str = ".migrated-from-senpi";
pub struct BrandDirMigrationResult { pub migrated: bool, pub copied: Vec<String>, pub from: PathBuf, pub to: PathBuf }
fn copy_tree(source: &Path, target: &Path) -> io::Result<()> {
    #[cfg(unix)]
    if fs::symlink_metadata(source)?.file_type().is_symlink() {
        let link = fs::read_link(source)?;
        let link = if link.is_absolute() { link } else { std::path::absolute(source.parent().unwrap_or(Path::new(".")))?.join(link) };
        return std::os::unix::fs::symlink(link, target);
    }
    if source.is_dir() { fs::create_dir_all(target)?; for entry in fs::read_dir(source)? { let entry = entry?; copy_tree(&entry.path(), &target.join(entry.file_name()))?; } }
    else { fs::copy(source, target)?; } Ok(())
}
pub fn migrate_engine_state_to_brand_dir(legacy: &Path, target: &Path) -> io::Result<BrandDirMigrationResult> {
    let mut result = BrandDirMigrationResult { migrated: false, copied: Vec::new(), from: legacy.to_owned(), to: target.to_owned() };
    if !legacy.exists() || target.join(MIGRATION_MARKER).exists() || target.join("settings.json").exists() { return Ok(result); }
    let Ok(entries) = fs::read_dir(legacy) else { return Ok(result); }; fs::create_dir_all(target)?;
    for entry in entries { let entry = entry?; let name = entry.file_name().to_string_lossy().into_owned(); if matches!(name.as_str(), "cache" | "logs" | "omo-local-update") || name.ends_with(".log") || target.join(&name).exists() { continue; } match copy_tree(&entry.path(), &target.join(&name)) { Ok(()) => result.copied.push(name), Err(error) => eprintln!("Brand migration could not copy entry: {error}") } }
    fs::write(target.join(MIGRATION_MARKER), format!("{}\n", legacy.display()))?; result.migrated = true; Ok(result)
}
