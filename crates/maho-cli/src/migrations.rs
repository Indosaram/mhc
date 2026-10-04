use std::fs;
use std::path::Path;
use serde_json::{Map, Value};
pub struct MigrationResult { pub migrated_auth_providers: Vec<String>, pub deprecation_warnings: Vec<String> }
pub fn run_migrations(cwd: &Path, home: &Path, agent: &Path) -> std::io::Result<MigrationResult> {
    if maho_core::config::config_flat_layout() {
        let migration = crate::brand_dir_migration::migrate_engine_state_to_brand_dir(
            &home.join(".senpi/agent"), &home.join(maho_core::config::config_dir_name()),
        )?;
        if migration.migrated && !migration.copied.is_empty() {
            println!("\x1b[32mCopied existing settings from {} to {}\x1b[39m", migration.from.display(), migration.to.display());
            println!("\x1b[2mThe original directory is untouched; the two installs keep separate state from now on.\x1b[22m");
        }
    }
    let migrated_auth_providers = migrate_auth_to_auth_json(agent)?;
    let completed = crate::migrations_state::read_completed_scan_migrations(agent);
    if !completed.contains("migrateLegacySenpiDirs") { crate::legacy_senpi_dir_migration::migrate_legacy_senpi_dirs(cwd, home, agent)?; }
    if !completed.contains("migrateSessionsFromAgentRoot") { migrate_sessions_from_agent_root(agent)?; }
    migrate_tools_to_bin(agent)?; migrate_keybindings_config_file(agent)?;
    let deprecation_warnings = crate::extension_system_migration::migrate_extension_system(cwd, agent);
    if completed.len() != crate::migrations_state::SCAN_MIGRATIONS.len() { crate::migrations_state::write_completed_scan_migrations(&crate::migrations_state::SCAN_MIGRATIONS, agent)?; }
    Ok(MigrationResult { migrated_auth_providers, deprecation_warnings })
}

pub fn show_deprecation_warnings(warnings: &[String]) -> std::io::Result<()> {
    use std::io::Write;
    if warnings.is_empty() { return Ok(()); }
    for warning in warnings { println!("\x1b[33mWarning: {warning}\x1b[39m"); }
    println!("\x1b[33m\nMove your extensions to the extensions/ directory.\x1b[39m");
    println!("\x1b[33mMigration guide: https://github.com/earendil-works/pi/blob/main/packages/coding-agent/CHANGELOG.md#extensions-migration\x1b[39m");
    println!("\x1b[33mDocumentation: https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/extensions.md\x1b[39m");
    println!("\x1b[2m\nPress any key to continue...\x1b[22m");
    std::io::stdout().flush()?;
    crossterm::terminal::enable_raw_mode()?;
    let result = loop {
        match crossterm::event::read() {
            Ok(crossterm::event::Event::Key(key)) if key.kind != crossterm::event::KeyEventKind::Release => break Ok(()),
            Ok(_) => {},
            Err(error) => break Err(error),
        }
    };
    let restored = crossterm::terminal::disable_raw_mode();
    result?;
    restored?;
    println!();
    Ok(())
}

fn read_json(path: &Path) -> Option<Value> { let text = fs::read_to_string(path).ok()?; serde_json::from_str(maho_core::text::strip_bom(&text)).ok() }
fn write_json(path: &Path, value: &Value, newline: bool) -> std::io::Result<()> { let mut text = serde_json::to_string_pretty(value)?; if newline { text.push('\n'); } fs::write(path, text) }
pub fn migrate_auth_to_auth_json(agent: &Path) -> std::io::Result<Vec<String>> {
    let auth = agent.join("auth.json"); if auth.exists() { return Ok(Vec::new()); }
    let mut migrated = Map::new(); let mut providers = Vec::new(); let oauth = agent.join("oauth.json"); let settings = agent.join("settings.json");
    if let Some(Value::Object(data)) = read_json(&oauth) { for (provider, credential) in data { let mut entry = Map::new(); entry.insert("type".to_owned(), Value::String("oauth".to_owned())); if let Value::Object(credential) = credential { entry.extend(credential); } providers.push(provider.clone()); migrated.insert(provider, Value::Object(entry)); } if let Err(error) = fs::rename(&oauth, agent.join("oauth.json.migrated")) { eprintln!("Migration could not rename oauth storage: {error}"); } }
    if let Some(mut value) = read_json(&settings) && let Some(keys) = value.get("apiKeys").and_then(Value::as_object) { for (provider, key) in keys { if !migrated.contains_key(provider) && key.is_string() { providers.push(provider.clone()); migrated.insert(provider.clone(), serde_json::json!({"type":"api_key", "key":key})); } } if let Some(object) = value.as_object_mut() { object.remove("apiKeys"); } write_json(&settings, &value, false)?; }
    if !migrated.is_empty() { fs::create_dir_all(agent)?; maho_core::auth_storage::write_auth_file(&auth.to_string_lossy(), &serde_json::to_string_pretty(&migrated)?).map_err(std::io::Error::other)?; }
    Ok(providers)
}
pub fn migrate_sessions_from_agent_root(agent: &Path) -> std::io::Result<()> {
    let Ok(entries) = fs::read_dir(agent) else { return Ok(()); };
    for entry in entries { let entry = entry?; if entry.path().extension().is_none_or(|ext| ext != "jsonl") { continue; }
        let Ok(text) = fs::read_to_string(entry.path()) else { continue; }; let Some(first) = text.lines().next() else { continue; }; let Ok(header) = serde_json::from_str::<Value>(first) else { continue; };
        if header.get("type").and_then(Value::as_str) != Some("session") { continue; }
        let Some(cwd) = header.get("cwd").and_then(Value::as_str).filter(|v| !v.is_empty()) else { continue; };
        let trimmed = cwd.strip_prefix('/').or_else(|| cwd.strip_prefix('\\')).unwrap_or(cwd);
        let safe: String = trimmed.chars().map(|c| if matches!(c, '/' | '\\' | ':') { '-' } else { c }).collect();
        let directory = agent.join("sessions").join(format!("--{safe}--")); let target = directory.join(entry.file_name()); if target.exists() { continue; } fs::create_dir_all(directory)?; if let Err(error) = fs::rename(entry.path(), target) { eprintln!("Session migration could not move file: {error}"); }
    } Ok(())
}
pub fn migrate_tools_to_bin(agent: &Path) -> std::io::Result<()> {
    let mut moved = false; for binary in ["fd", "rg", "fd.exe", "rg.exe"] { let old = agent.join("tools").join(binary); if !old.exists() { continue; } let target = agent.join("bin").join(binary); fs::create_dir_all(agent.join("bin"))?; if target.exists() { fs::remove_file(old)?; } else { fs::rename(old, target)?; moved = true; } }
    if moved { println!("Migrated managed binaries tools/ → bin/"); } Ok(())
}
pub fn migrate_keybindings_config_file(agent: &Path) -> std::io::Result<()> {
    let path = agent.join("keybindings.json"); let Some(Value::Object(raw)) = read_json(&path) else { return Ok(()); }; let result = maho_core::keybindings::migrate_keybindings_config(&raw); if result.migrated { write_json(&path, &Value::Object(result.config), true)?; } Ok(())
}
