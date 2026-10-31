//! Read and write the engine's `<agentDir>/auth.json` (upstream `omo-native/bin/lib/auth-store.js`):
//! every setup stage that imports a key goes through here, so they all escape, back up and write it
//! the same way.

use serde_json::{Map, Value, json};
use std::path::Path;

/// opencode keeps a pasted key verbatim and never interprets it, but the engine resolves every
/// stored key as a config value: a leading `!` runs a shell command and `$NAME` / `${NAME}`
/// interpolate the environment. `$$` and `$!` are the engine's literal escapes, so the engine reads
/// back the exact bytes opencode held.
pub fn literal_config_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for character in value.chars() {
        if character == '$' || character == '!' {
            out.push('$');
        }
        out.push(character);
    }
    out
}

/// `readAuthStore`: the parsed entries and the bytes they came from, or `malformed`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AuthStore {
    pub entries: Map<String, Value>,
    pub bytes: Option<String>,
    pub malformed: bool,
}

pub fn read_auth_store(path: &Path) -> AuthStore {
    let Ok(bytes) = std::fs::read_to_string(path) else {
        return AuthStore::default();
    };
    match serde_json::from_str::<Value>(&bytes) {
        Ok(Value::Object(entries)) => AuthStore {
            entries,
            bytes: Some(bytes),
            malformed: false,
        },
        _ => AuthStore {
            entries: Map::new(),
            bytes: Some(bytes),
            malformed: true,
        },
    }
}

/// A key to store for `provider`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyAddition {
    pub provider: String,
    pub key: String,
}

// The credential and custom-provider stages of one run can both rewrite auth.json within the same
// millisecond. The first backup holds the older bytes, so a later one never replaces it.
fn backup(path: &Path) -> std::io::Result<()> {
    let target = format!("{}.bak-{}", path.display(), super::timestamp());
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&target)
    {
        Ok(_) => std::fs::copy(path, &target).map(|_| ()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error),
    }
}

/// Backs up `path` (when it existed) and writes `current` plus `additions` with mode 0600.
///
/// # Errors
/// A filesystem step failed.
pub fn write_auth_store(
    path: &Path,
    current: &AuthStore,
    additions: &[KeyAddition],
) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        super::create_private_dir(parent)?;
    }
    if current.bytes.is_some() {
        backup(path)?;
    }
    let mut next = current.entries.clone();
    for item in additions {
        next.insert(
            item.provider.clone(),
            json!({"type": "api_key", "key": item.key}),
        );
    }
    let text = serde_json::to_string_pretty(&Value::Object(next)).map_err(std::io::Error::other)?;
    super::write_private(path, text.as_bytes())?;
    super::set_private(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_values_escape_dollar_and_bang() {
        assert_eq!(literal_config_value("!cmd $HOME ok"), "$!cmd $$HOME ok");
    }

    #[test]
    fn write_keeps_entries_backs_up_and_reads_back() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("agent/auth.json");
        assert_eq!(read_auth_store(&path), AuthStore::default());
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, r#"{"anthropic":{"type":"oauth"}}"#).expect("write");
        let current = read_auth_store(&path);
        write_auth_store(
            &path,
            &current,
            &[KeyAddition {
                provider: "openai".into(),
                key: "k".into(),
            }],
        )
        .expect("write");
        let after = read_auth_store(&path);
        assert_eq!(
            after.entries["openai"],
            json!({"type": "api_key", "key": "k"})
        );
        assert_eq!(after.entries["anthropic"], json!({"type": "oauth"}));
        let backups = std::fs::read_dir(path.parent().expect("parent"))
            .expect("dir")
            .filter_map(Result::ok)
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("auth.json.bak-")
            })
            .count();
        assert_eq!(backups, 1);
        std::fs::write(&path, "[1]").expect("write");
        assert!(read_auth_store(&path).malformed);
    }
}
