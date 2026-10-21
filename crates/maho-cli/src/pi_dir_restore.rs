use crate::legacy_dir_copy::{
    copy_missing_entries, is_regenerable_entry, is_within_or_same_path,
    paths_point_to_same_location,
};
use crate::migrations_state::{MIGRATIONS_STATE_FILENAME, record_legacy_pi_agent_dir_copy};
use std::{collections::BTreeSet, fs, io, path::Path};

const UPSTREAM_PI_AGENT_ENTRIES: [&str; 17] = [
    "auth.json",
    "settings.json",
    "models.json",
    "models-store.json",
    "keybindings.json",
    "sessions",
    "extensions",
    "skills",
    "prompts",
    "themes",
    "npm",
    "git",
    "experimental",
    "AGENTS.md",
    "CLAUDE.md",
    "SYSTEM.md",
    "APPEND_SYSTEM.md",
];

fn is_empty_json_stub(path: &Path) -> io::Result<bool> {
    let stat = fs::symlink_metadata(path)?;
    if !stat.is_file() || stat.len() > 16 {
        return Ok(false);
    }
    let bytes = fs::read(path)?;
    Ok(matches!(
        String::from_utf8_lossy(&bytes).trim(),
        "{}" | "[]"
    ))
}
fn holds_no_user_state(dir: &Path) -> io::Result<bool> {
    if !dir.exists() {
        return Ok(true);
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if !is_regenerable_entry(&entry.file_name().to_string_lossy())
            && !is_empty_json_stub(&entry.path())?
        {
            return Ok(false);
        }
    }
    Ok(true)
}
fn restore_dir(from: &Path, to: &Path, include: impl Fn(&str) -> bool) -> io::Result<bool> {
    if !from.is_dir() || !holds_no_user_state(to)? || paths_point_to_same_location(from, to)? {
        return Ok(false);
    }
    if to.exists() {
        for entry in fs::read_dir(to)? {
            let entry = entry?;
            if include(&entry.file_name().to_string_lossy())
                && from.join(entry.file_name()).exists()
                && is_empty_json_stub(&entry.path())?
            {
                fs::remove_file(entry.path())?;
            }
        }
    }
    let restored = copy_missing_entries(from, to, include)?;
    if !restored.is_empty() {
        crate::cli::stdout_guard::write_line(&format!(
            "Restored {} from {}: an earlier start had moved it there.\nBoth directories now hold independent copies.\n",
            to.display(),
            from.display()
        ));
    }
    Ok(!restored.is_empty())
}
pub fn restore_drained_pi_dirs(
    completed: &BTreeSet<String>,
    home: &Path,
    agent: &Path,
) -> io::Result<()> {
    let config = maho_core::config::config_dir_name();
    let brand = home.join(&config);
    let pi = home.join(".pi");
    if config == ".pi" || !is_within_or_same_path(agent, &brand)? || !pi.is_dir() {
        return Ok(());
    }
    if !completed.contains("migrateLegacySenpiDirs") {
        if agent.join(MIGRATIONS_STATE_FILENAME).exists() || !agent.is_dir() {
            return Ok(());
        }
        let mut evidence = false;
        for entry in fs::read_dir(agent)? {
            if UPSTREAM_PI_AGENT_ENTRIES.contains(&entry?.file_name().to_string_lossy().as_ref()) {
                evidence = true;
                break;
            }
        }
        if !evidence {
            return Ok(());
        }
    }
    let result: io::Result<()> = (|| {
        if restore_dir(agent, &pi.join("agent"), |entry| {
            UPSTREAM_PI_AGENT_ENTRIES.contains(&entry)
        })? {
            record_legacy_pi_agent_dir_copy(agent)?;
        }
        restore_dir(&brand.join("mom"), &pi.join("mom"), |entry| {
            !is_regenerable_entry(entry)
        })?;
        Ok(())
    })();
    if let Err(error) = result {
        eprintln!("Could not restore {}: {error}", pi.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restores_only_pi_entries_as_independent_copies_into_stub_directory() {
        // Given
        let root = tempfile::tempdir().expect("tempdir");
        let agent = root.path().join(".omo/agent");
        let pi = root.path().join(".pi/agent");
        fs::create_dir_all(&agent).expect("agent");
        fs::create_dir_all(&pi).expect("pi");
        fs::write(agent.join("settings.json"), "user settings").expect("settings");
        fs::write(agent.join("private-state"), "native only").expect("private");
        fs::write(pi.join("settings.json"), "{}").expect("stub");
        let completed = BTreeSet::from(["migrateLegacySenpiDirs".to_owned()]);
        // When
        restore_drained_pi_dirs(&completed, root.path(), &agent).expect("restore");
        // Then
        assert_eq!(
            fs::read(pi.join("settings.json")).expect("restored"),
            b"user settings"
        );
        assert!(!pi.join("private-state").exists());
        fs::write(pi.join("settings.json"), "independent").expect("edit pi");
        assert_eq!(
            fs::read(agent.join("settings.json")).expect("agent"),
            b"user settings"
        );
        assert!(
            crate::migrations_state::read_legacy_pi_agent_dir_record(&agent)
                .copied_at
                .is_some()
        );
    }

    #[test]
    fn restoration_does_not_merge_into_pi_with_user_state() {
        // Given
        let root = tempfile::tempdir().expect("tempdir");
        let agent = root.path().join(".omo/agent");
        let pi = root.path().join(".pi/agent");
        fs::create_dir_all(&agent).expect("agent");
        fs::create_dir_all(&pi).expect("pi");
        fs::write(agent.join("settings.json"), "native").expect("settings");
        fs::write(pi.join("auth.json"), "user state").expect("auth");
        // When
        restore_drained_pi_dirs(
            &BTreeSet::from(["migrateLegacySenpiDirs".to_owned()]),
            root.path(),
            &agent,
        )
        .expect("restore");
        // Then
        assert!(!pi.join("settings.json").exists());
        assert_eq!(fs::read(pi.join("auth.json")).expect("auth"), b"user state");
    }
}
