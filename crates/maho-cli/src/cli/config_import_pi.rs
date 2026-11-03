//! Port of senpi `cli/config-import-pi.ts` (pin 903b8336).

/// senpi `CONFIG_IMPORT_PI_ARGV`: the `config` subcommand that imports Pi config files.
pub const CONFIG_IMPORT_PI_ARGV: &str = "import-pi";

/// senpi `CONFIG_IMPORT_PI_USAGE`.
pub fn config_import_pi_usage() -> String {
    format!(
        "{} config {CONFIG_IMPORT_PI_ARGV} [{} ...]",
        maho_core::config::app_command(),
        crate::legacy_pi_edits::LEGACY_PI_CONFIG_FILES.join("|")
    )
}

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
        maho_core::output_guard::maho_write_stderr(&format!(
            "Skipped {}: {}\n",
            failure.file, failure.reason
        ));
    }
    if result.failures.is_empty() {
        Ok(())
    } else {
        Err("Some Pi config files could not be imported".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_names_the_subcommand_and_every_importable_file() {
        let usage = config_import_pi_usage();
        assert!(usage.ends_with(
            " config import-pi [auth.json|keybindings.json|models.json|settings.json ...]"
        ));
        assert!(usage.starts_with(&maho_core::config::app_command()));
    }
}
