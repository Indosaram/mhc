use crate::legacy_dir_copy::{
    copy_missing_entries, is_regenerable_entry, is_within_or_same_path,
    paths_point_to_same_location,
};
use std::{fs, io, path::Path};
/// senpi `copyLegacyDir`: an official `.pi` dir belongs to a pi install that keeps running, so its
/// non-regenerable entries are only ever copied; the source stays untouched.
pub fn migrate_path_preserving_existing(old: &Path, new: &Path, label: &str) -> io::Result<()> {
    copy_legacy_dir(old, new, label, false)
}

fn copy_legacy_dir(old: &Path, new: &Path, label: &str, agent_config: bool) -> io::Result<()> {
    if !old.exists() || paths_point_to_same_location(old, new)? {
        return Ok(());
    }
    let copied = copy_missing_entries(old, new, |name| !is_regenerable_entry(name))?;
    if !copied.is_empty() {
        crate::cli::stdout_guard::write_line(&format!(
            "Copied {label} {} → {}\n",
            old.display(),
            new.display()
        ));
        if agent_config {
            crate::migrations_state::record_legacy_pi_agent_dir_copy(new)?;
            let app = maho_core::config::app_name();
            crate::cli::stdout_guard::write_line(&format!(
                "{app} reads its config from {} now; edits to {} no longer reach {app}.\n",
                new.display(),
                old.display()
            ));
        } else {
            crate::cli::stdout_guard::write_line(
                "The original directory is untouched; the two installs keep separate state from now on.\n",
            );
        }
    }
    Ok(())
}
/// senpi `moveLegacyDir`: a `.pi` leftover nested in our own config dir is moved.
fn move_legacy_dir(old: &Path, new: &Path, label: &str) -> io::Result<()> {
    if !old.exists() || paths_point_to_same_location(old, new)? {
        return Ok(());
    }
    if !new.exists() {
        if let Some(parent) = new.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::rename(old, new)?;
        crate::cli::stdout_guard::write_line(&format!(
            "Migrated {label} {} → {}\n",
            old.display(),
            new.display()
        ));
        return Ok(());
    }
    let mut moved = false;
    for entry in fs::read_dir(old)? {
        let entry = entry?;
        let target = new.join(entry.file_name());
        if target.exists() {
            continue;
        }
        fs::rename(entry.path(), target)?;
        moved = true;
    }
    if moved {
        crate::cli::stdout_guard::write_line(&format!(
            "Migrated missing {label} entries {} → {}\n",
            old.display(),
            new.display()
        ));
    }
    Ok(())
}
pub fn migrate_legacy_senpi_dirs(cwd: &Path, home: &Path, agent: &Path) -> io::Result<()> {
    let config_dir = maho_core::config::config_dir_name();
    if config_dir == ".pi" {
        return Ok(());
    }
    let config = home.join(&config_dir);
    let project = cwd.join(&config_dir);
    // (from, to, label, move): official `.pi` dirs are copied, `.pi` leftovers nested in our config dir are moved.
    let mut dirs = vec![
        (
            cwd.join(".pi"),
            project.clone(),
            "project config directory",
            false,
        ),
        (
            project.join(".pi"),
            project,
            "nested project config directory",
            true,
        ),
    ];
    if is_within_or_same_path(agent, &config)? {
        let mut globals = vec![
            (
                home.join(".pi/agent"),
                agent.to_owned(),
                "global agent directory",
                false,
            ),
            (
                config.join(".pi/agent"),
                agent.to_owned(),
                "nested global agent directory",
                true,
            ),
            (
                home.join(".pi/mom"),
                config.join("mom"),
                "global mom directory",
                false,
            ),
            (
                config.join(".pi/mom"),
                config.join("mom"),
                "nested global mom directory",
                true,
            ),
        ];
        globals.append(&mut dirs);
        dirs = globals;
    }
    for (old, new, label, moves) in dirs {
        let result = if moves {
            move_legacy_dir(&old, &new, label)
        } else {
            copy_legacy_dir(&old, &new, label, old == home.join(".pi/agent"))
        };
        if let Err(error) = result {
            eprintln!("Skipped the {label} {}: {error}", old.display());
        }
    }
    Ok(())
}
