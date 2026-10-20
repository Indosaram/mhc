use std::{fs, path::Path};
pub fn migrate_extension_system(cwd: &Path, agent: &Path) -> Vec<String> {
    let mut warnings = Vec::new();
    for (base, label) in [
        (agent.to_owned(), "Global"),
        (cwd.join(maho_core::config::config_dir_name()), "Project"),
    ] {
        let commands = base.join("commands");
        let prompts = base.join("prompts");
        if commands.exists() && !prompts.exists() {
            match fs::rename(commands, prompts) {
                Ok(()) => crate::cli::stdout_guard::write_line(&format!(
                    "Migrated {label} commands/ → prompts/\n"
                )),
                Err(error) => {
                    eprintln!("Warning: Could not migrate {label} commands/ to prompts/: {error}")
                }
            }
        }
        if base.join("hooks").exists() {
            warnings.push(format!(
                "{label} hooks/ directory found. Hooks have been renamed to extensions."
            ));
        }
        if let Ok(entries) = fs::read_dir(base.join("tools"))
            && entries
                .filter_map(Result::ok)
                .any(|entry| entry.path().join("index.ts").exists())
        {
            warnings.push(format!("{label} tools/ directory contains custom tools. Custom tools have been merged into extensions."));
        }
    }
    warnings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignores_entries_when_custom_tool_entry_point_is_absent() {
        // Given: unrelated directories, loose source files and managed binaries.
        let root = tempfile::tempdir().expect("temporary migration root");
        let cwd = root.path().join("project");
        let agent = root.path().join("agent");
        for base in [
            agent.clone(),
            cwd.join(maho_core::config::config_dir_name()),
        ] {
            let tools = base.join("tools");
            fs::create_dir_all(tools.join("unrelated")).expect("unrelated directory");
            for name in [
                "notes.txt",
                "index.ts",
                "custom.ts",
                "fd",
                "rg",
                "fd.exe",
                "rg.exe",
            ] {
                fs::write(tools.join(name), b"").expect("unrelated file");
            }
        }

        // When: both extension locations are scanned.
        let warnings = migrate_extension_system(&cwd, &agent);

        // Then: no entry is mistaken for a deprecated custom tool.
        assert!(warnings.is_empty());
    }

    #[test]
    fn detects_global_custom_tool_when_entry_is_hidden() {
        // Given: a hidden custom tool with the upstream entry point.
        let root = tempfile::tempdir().expect("temporary migration root");
        let cwd = root.path().join("project");
        let agent = root.path().join("agent");
        let tool = agent.join("tools/.custom");
        fs::create_dir_all(&tool).expect("custom tool directory");
        fs::write(tool.join("index.ts"), b"").expect("custom tool entry point");

        // When: the global extension location is scanned.
        let warnings = migrate_extension_system(&cwd, &agent);

        // Then: hidden names do not suppress custom-tool detection.
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn detects_project_custom_tools_when_entries_have_managed_binary_names() {
        // Given: multiple custom tools named like managed binaries.
        let root = tempfile::tempdir().expect("temporary migration root");
        let cwd = root.path().join("project");
        let agent = root.path().join("agent");
        let tools = cwd.join(maho_core::config::config_dir_name()).join("tools");
        for name in ["fd", "rg", "fd.exe", "rg.exe"] {
            fs::create_dir_all(tools.join(name)).expect("custom tool directory");
            fs::write(tools.join(name).join("index.ts"), b"").expect("custom tool entry point");
        }

        // When: the project extension location is scanned.
        let warnings = migrate_extension_system(&cwd, &agent);

        // Then: entry points produce one warning regardless of name or count.
        assert_eq!(warnings.len(), 1);
    }
}
