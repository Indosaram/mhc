use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::Path;
use maho_core::keybindings::KeybindingsManager;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeybindingsEditResult { Reloaded, Invalid { message: String } }

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
