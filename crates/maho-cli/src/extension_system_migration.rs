use std::{fs, path::Path};
pub fn migrate_extension_system(cwd: &Path, agent: &Path) -> Vec<String> {
    let mut warnings = Vec::new();
    for (base, label) in [(agent.to_owned(), "Global"), (cwd.join(".maho"), "Project")] {
        let commands = base.join("commands"); let prompts = base.join("prompts");
        if commands.exists() && !prompts.exists() { match fs::rename(commands, prompts) { Ok(()) => crate::cli::stdout_guard::write_line(&format!("Migrated {label} commands/ → prompts/\n")), Err(error) => eprintln!("Warning: Could not migrate {label} commands/ to prompts/: {error}") } }
        if base.join("hooks").exists() { warnings.push(format!("{label} hooks/ directory found. Hooks have been renamed to extensions.")); }
        if let Ok(entries) = fs::read_dir(base.join("tools")) && entries.filter_map(Result::ok).any(|entry| { let name = entry.file_name().to_string_lossy().into_owned(); !name.starts_with('.') && !matches!(name.to_lowercase().as_str(), "fd" | "rg" | "fd.exe" | "rg.exe") }) { warnings.push(format!("{label} tools/ directory contains custom tools. Custom tools have been merged into extensions.")); }
    } warnings
}
