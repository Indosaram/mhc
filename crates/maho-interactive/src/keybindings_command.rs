use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::Path;
use maho_core::keybindings::KeybindingsManager;

use crate::external_editor::{EditFileResult, edit_file_in_external_editor};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeybindingsEditResult { Reloaded, Invalid { message: String } }

/// senpi's `process.env.VISUAL || process.env.EDITOR`: the first non-blank value wins.
pub fn resolve_editor_command(env: &std::collections::BTreeMap<String, String>) -> Option<String> {
    [env.get("VISUAL"), env.get("EDITOR")].into_iter().flatten().find(|value| !value.trim().is_empty()).cloned()
}

/// Outcome of `/keybindings`' external-editor round trip (senpi `handleKeybindingsCommand`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeybindingsEditOutcome {
    /// The editor ran and the file was applied to the live manager.
    Reloaded,
    /// The editor command never launched. `seeded` says whether the command created the file.
    LaunchFailed { seeded: bool },
    /// The editor ran and exited non-zero; the on-disk file is left as written.
    Exited { code: i32 },
    /// The file is not valid JSON, so the live manager was not reloaded.
    Invalid { message: String },
    IoError { message: String },
}

/// Seed the keybindings file if absent, hand it to the configured editor, then apply it.
pub async fn run_keybindings_edit(config_path: &Path, editor_command: &str, keybindings: &mut KeybindingsManager) -> KeybindingsEditOutcome {
    let seeded = match seed_keybindings_file(config_path, keybindings) {
        Ok(seeded) => seeded,
        Err(error) => return KeybindingsEditOutcome::IoError { message: error.to_string() },
    };
    match edit_file_in_external_editor(editor_command, config_path).await {
        Ok(EditFileResult::LaunchFailed) => KeybindingsEditOutcome::LaunchFailed { seeded },
        Ok(EditFileResult::Exited { code }) => KeybindingsEditOutcome::Exited { code },
        Ok(EditFileResult::Complete) => match apply_keybindings_file_edit(config_path, keybindings) {
            KeybindingsEditResult::Reloaded => KeybindingsEditOutcome::Reloaded,
            KeybindingsEditResult::Invalid { message } => KeybindingsEditOutcome::Invalid { message },
        },
        Err(error) => KeybindingsEditOutcome::IoError { message: error.to_string() },
    }
}

pub fn seed_keybindings_file(path: &Path, keybindings: &KeybindingsManager) -> io::Result<bool> {
    let mut file = match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Ok(false),
        Err(error) => return Err(error),
    };
    let config: serde_json::Map<String, serde_json::Value> = keybindings.get_effective_config().into_iter().filter_map(|(id, keys)| {
        keys.map(|keys| {
            let value = match keys {
                maho_tui::keybindings::Keys::One(key) => serde_json::Value::String(key),
                maho_tui::keybindings::Keys::Many(keys) => serde_json::Value::Array(keys.into_iter().map(serde_json::Value::String).collect()),
            };
            (id, value)
        })
    }).collect();
    let raw = serde_json::to_string_pretty(&config)?;
    writeln!(file, "{raw}")?;
    Ok(true)
}

pub fn apply_keybindings_file_edit(path: &Path, keybindings: &mut KeybindingsManager) -> KeybindingsEditResult {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) => return KeybindingsEditResult::Invalid { message: error.to_string() },
    };
    if let Err(error) = serde_json::from_str::<serde_json::Value>(&raw) {
        return KeybindingsEditResult::Invalid { message: error.to_string() };
    }
    keybindings.reload();
    KeybindingsEditResult::Reloaded
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::os::unix::fs::PermissionsExt;

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(key, value)| ((*key).to_owned(), (*value).to_owned())).collect()
    }

    fn editor_script(directory: &Path, body: &str) -> String {
        let path = directory.join("editor.sh");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("script");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("executable");
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn editor_resolution_prefers_visual_and_skips_blank_values() {
        assert_eq!(resolve_editor_command(&env(&[("VISUAL", "vis"), ("EDITOR", "ed")])).as_deref(), Some("vis"));
        assert_eq!(resolve_editor_command(&env(&[("VISUAL", "   "), ("EDITOR", "ed")])).as_deref(), Some("ed"));
        assert_eq!(resolve_editor_command(&env(&[("EDITOR", "ed")])).as_deref(), Some("ed"));
        assert_eq!(resolve_editor_command(&env(&[])), None);
    }

    #[tokio::test]
    async fn editor_round_trip_seeds_the_file_and_reloads_valid_json() {
        let directory = tempfile::tempdir().expect("directory");
        let config = directory.path().join("keybindings.json");
        let editor = editor_script(directory.path(), r#"printf '{"tui.input.submit":"ctrl+j"}' > "$1""#);
        let mut keybindings = KeybindingsManager::create(None);
        let outcome = run_keybindings_edit(&config, &editor, &mut keybindings).await;
        assert_eq!(outcome, KeybindingsEditOutcome::Reloaded);
        assert_eq!(std::fs::read_to_string(&config).expect("written file"), r#"{"tui.input.submit":"ctrl+j"}"#);
    }

    #[tokio::test]
    async fn nonzero_editor_exit_leaves_the_written_file_untouched() {
        let directory = tempfile::tempdir().expect("directory");
        let config = directory.path().join("keybindings.json");
        let editor = editor_script(directory.path(), "exit 3");
        let mut keybindings = KeybindingsManager::create(None);
        let outcome = run_keybindings_edit(&config, &editor, &mut keybindings).await;
        assert_eq!(outcome, KeybindingsEditOutcome::Exited { code: 3 });
        assert!(config.exists());
    }

    #[tokio::test]
    async fn invalid_json_is_reported_without_reloading() {
        let directory = tempfile::tempdir().expect("directory");
        let config = directory.path().join("keybindings.json");
        let editor = editor_script(directory.path(), r#"printf 'not json' > "$1""#);
        let mut keybindings = KeybindingsManager::create(None);
        let outcome = run_keybindings_edit(&config, &editor, &mut keybindings).await;
        assert!(matches!(outcome, KeybindingsEditOutcome::Invalid { .. }));
    }

    #[tokio::test]
    async fn missing_editor_binary_reports_launch_failure_with_the_seeded_flag() {
        let directory = tempfile::tempdir().expect("directory");
        let config = directory.path().join("keybindings.json");
        let missing = directory.path().join("does-not-exist").to_string_lossy().into_owned();
        let mut keybindings = KeybindingsManager::create(None);
        let outcome = run_keybindings_edit(&config, &missing, &mut keybindings).await;
        assert_eq!(outcome, KeybindingsEditOutcome::LaunchFailed { seeded: true });
        assert!(config.exists(), "the seed survives until the caller decides to remove it");
    }
}
