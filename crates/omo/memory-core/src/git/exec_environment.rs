//! Environment filtering for memory repository Git commands (#12354).

use std::collections::{BTreeMap, BTreeSet};

use super::exec::GitPlatform;

const REPOSITORY_LOCAL_GIT_ENV: &[&str] = &[
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CONFIG",
    "GIT_OBJECT_DIRECTORY",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_IMPLICIT_WORK_TREE",
    "GIT_GRAFT_FILE",
    "GIT_NO_REPLACE_OBJECTS",
    "GIT_REPLACE_REF_BASE",
    "GIT_PREFIX",
    "GIT_SHALLOW_FILE",
    "GIT_COMMON_DIR",
    "GIT_NAMESPACE",
];

const CONFIG_COUNT: &str = "GIT_CONFIG_COUNT";
const CONFIG_PARAMETERS: &str = "GIT_CONFIG_PARAMETERS";

fn is_numbered_config_pair(key: &str) -> bool {
    let suffix = key
        .strip_prefix("GIT_CONFIG_KEY_")
        .or_else(|| key.strip_prefix("GIT_CONFIG_VALUE_"));
    matches!(suffix, Some(rest) if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
}

fn defeats_memory_hooks(key: &str) -> bool {
    let name = key.trim();
    let Some(first_dot) = name.find('.') else {
        return false;
    };
    let Some(last_dot) = name.rfind('.') else {
        return false;
    };
    if first_dot == 0 || last_dot + 1 >= name.len() {
        return false;
    }
    let section = name[..first_dot].to_ascii_lowercase();
    let variable = name[last_dot + 1..].to_ascii_lowercase();
    if section == "core" {
        return first_dot == last_dot && (variable == "hookspath" || variable == "fsmonitor");
    }
    variable == "path" && (section == "include" || section == "includeif")
}

fn config_count_pairs(by_git_name: &BTreeMap<String, String>) -> Option<Vec<(String, String)>> {
    let Some(raw) = by_git_name.get(CONFIG_COUNT) else {
        return Some(Vec::new());
    };
    let trimmed = raw.trim();
    let count: usize = if trimmed.is_empty() {
        0
    } else {
        let digits = trimmed.strip_prefix('+').unwrap_or(trimmed);
        if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        digits.parse().ok()?
    };
    let mut pairs = Vec::with_capacity(count.min(1024));
    for index in 0..count {
        let key = by_git_name.get(&format!("GIT_CONFIG_KEY_{index}"))?;
        let value = by_git_name.get(&format!("GIT_CONFIG_VALUE_{index}"))?;
        pairs.push((key.clone(), value.clone()));
    }
    Some(pairs)
}

fn dequote_step(text: &str, start: usize) -> Option<(String, usize)> {
    let bytes = text.as_bytes();
    if bytes.get(start) != Some(&b'\'') {
        return None;
    }
    let mut value = String::new();
    let mut at = start + 1;
    loop {
        let quote = text[at..].find('\'').map(|rel| at + rel)?;
        value.push_str(&text[at..quote]);
        if bytes.get(quote + 1) == Some(&b'\\')
            && matches!(bytes.get(quote + 2), Some(b'\'' | b'!'))
            && bytes.get(quote + 3) == Some(&b'\'')
        {
            value.push(bytes[quote + 2] as char);
            at = quote + 4;
            continue;
        }
        return Some((value, quote + 1));
    }
}

struct ConfigParamEntry<'a> {
    source: &'a str,
    key: String,
}

fn config_parameter_entries(text: &str) -> Option<Vec<ConfigParamEntry<'_>>> {
    let bytes = text.as_bytes();
    let mut entries = Vec::new();
    let mut at = 0usize;
    while at < text.len() {
        let (mut name, mut end) = dequote_step(text, at)?;
        if bytes.get(end) == Some(&b'=') {
            end += 1;
            if bytes.get(end) == Some(&b'\'') {
                let (_, value_end) = dequote_step(text, end)?;
                end = value_end;
            }
        } else if let Some(eq) = name.find('=') {
            name.truncate(eq);
        }
        if end < text.len() && !text[end..].chars().next().is_some_and(char::is_whitespace) {
            return None;
        }
        entries.push(ConfigParamEntry {
            source: &text[at..end],
            key: name,
        });
        while let Some(ch) = text[end..].chars().next() {
            if !ch.is_whitespace() {
                break;
            }
            end += ch.len_utf8();
        }
        at = end;
    }
    Some(entries)
}

/// Filter `environment` so repository redirection and hook-defeating git config never reach memory git
/// (`#12354`), while keeping `GIT_INDEX_FILE` and user credential/identity variables.
pub fn memory_git_environment(
    environment: &BTreeMap<String, String>,
    platform: GitPlatform,
) -> BTreeMap<String, String> {
    let blocked: BTreeSet<&str> = REPOSITORY_LOCAL_GIT_ENV.iter().copied().collect();
    let git_name = |name: &str| -> String {
        if platform == GitPlatform::Windows {
            name.to_ascii_uppercase()
        } else {
            name.to_owned()
        }
    };

    let mut kept = BTreeMap::new();
    let mut by_git_name = BTreeMap::new();
    for (name, value) in environment {
        let key = git_name(name);
        by_git_name
            .entry(key.clone())
            .or_insert_with(|| value.clone());
        if blocked.contains(key.as_str())
            || is_numbered_config_pair(&key)
            || key == CONFIG_COUNT
            || key == CONFIG_PARAMETERS
        {
            continue;
        }
        kept.insert(name.clone(), value.clone());
    }

    if let Some(pairs) = config_count_pairs(&by_git_name) {
        let filtered: Vec<(String, String)> = pairs
            .into_iter()
            .filter(|(key, _)| !defeats_memory_hooks(key))
            .collect();
        if !filtered.is_empty() {
            kept.insert(CONFIG_COUNT.to_owned(), filtered.len().to_string());
            for (index, (key, value)) in filtered.into_iter().enumerate() {
                kept.insert(format!("GIT_CONFIG_KEY_{index}"), key);
                kept.insert(format!("GIT_CONFIG_VALUE_{index}"), value);
            }
        }
    }

    if let Some(parameters) = by_git_name.get(CONFIG_PARAMETERS)
        && let Some(entries) = config_parameter_entries(parameters)
    {
        let filtered: Vec<&str> = entries
            .into_iter()
            .filter(|entry| !defeats_memory_hooks(&entry.key))
            .map(|entry| entry.source)
            .collect();
        if !filtered.is_empty() {
            kept.insert(CONFIG_PARAMETERS.to_owned(), filtered.join(" "));
        }
    }

    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_repository_redirection_and_hook_defeating_config_but_keeps_index_and_credentials() {
        let env = BTreeMap::from([
            ("PATH".to_owned(), "/usr/bin".to_owned()),
            ("GIT_DIR".to_owned(), "/tmp/other/.git".to_owned()),
            ("GIT_WORK_TREE".to_owned(), "/tmp/other".to_owned()),
            ("GIT_NAMESPACE".to_owned(), "ns".to_owned()),
            ("GIT_INDEX_FILE".to_owned(), "/tmp/tmp-index".to_owned()),
            ("GIT_CONFIG_COUNT".to_owned(), "3".to_owned()),
            ("GIT_CONFIG_KEY_0".to_owned(), "core.hooksPath".to_owned()),
            ("GIT_CONFIG_VALUE_0".to_owned(), "/dev/null".to_owned()),
            ("GIT_CONFIG_KEY_1".to_owned(), "safe.directory".to_owned()),
            ("GIT_CONFIG_VALUE_1".to_owned(), "*".to_owned()),
            (
                "GIT_CONFIG_KEY_2".to_owned(),
                "includeIf.gitdir:/tmp/.path".to_owned(),
            ),
            ("GIT_CONFIG_VALUE_2".to_owned(), "/tmp/evil".to_owned()),
            (
                "GIT_CONFIG_PARAMETERS".to_owned(),
                "'core.fsmonitor'='true' 'http.extraHeader'='X-Test: 1'".to_owned(),
            ),
        ]);
        let filtered = memory_git_environment(&env, GitPlatform::Other);
        assert!(!filtered.contains_key("GIT_DIR"));
        assert!(!filtered.contains_key("GIT_WORK_TREE"));
        assert!(!filtered.contains_key("GIT_NAMESPACE"));
        assert_eq!(
            filtered.get("GIT_INDEX_FILE").map(String::as_str),
            Some("/tmp/tmp-index")
        );
        assert_eq!(
            filtered.get("GIT_CONFIG_COUNT").map(String::as_str),
            Some("1")
        );
        assert_eq!(
            filtered.get("GIT_CONFIG_KEY_0").map(String::as_str),
            Some("safe.directory")
        );
        assert_eq!(
            filtered.get("GIT_CONFIG_PARAMETERS").map(String::as_str),
            Some("'http.extraHeader'='X-Test: 1'")
        );
    }

    #[test]
    fn handles_case_insensitive_names_on_windows() {
        let env = BTreeMap::from([
            ("Git_Dir".to_owned(), "C:\\other\\.git".to_owned()),
            ("PATH".to_owned(), "C:\\Windows".to_owned()),
        ]);
        let filtered = memory_git_environment(&env, GitPlatform::Windows);
        assert!(!filtered.contains_key("Git_Dir"));
        assert_eq!(
            filtered.get("PATH").map(String::as_str),
            Some("C:\\Windows")
        );
    }
}
