pub fn run_config_import_pi(files: &[String]) -> Result<(), String> {
    let agent = maho_core::config::get_agent_dir();
    let home = maho_core::config::home_dir();
    let result = crate::legacy_pi_edits::import_legacy_pi_config(
        files,
        std::path::Path::new(&agent),
        std::path::Path::new(&home),
        std::time::SystemTime::now(),
    )
    .map_err(|error| error.to_string())?;
    if result.imported.is_empty() && result.failures.is_empty() {
        crate::cli::stdout_guard::write_line(&format!(
            "Nothing to import: no config file in {} changed after it was copied to {}.\n",
            result.pi_agent_dir.display(),
            result.agent_dir.display()
        ));
    }
    for entry in result.imported {
        crate::cli::stdout_guard::write_line(&format!(
            "Imported {} → {}\n",
            entry.from.display(),
            entry.to.display()
        ));
        if let Some(backup) = entry.backup_path {
            crate::cli::stdout_guard::write_line(&format!(
                "The previous {} is saved as {}\n",
                entry.file,
                backup.display()
            ));
        }
    }
    for failure in &result.failures {
        eprintln!("Skipped {}: {}", failure.file, failure.reason);
    }
    if result.failures.is_empty() {
        Ok(())
    } else {
        Err("Some Pi config files could not be imported".to_owned())
    }
}
