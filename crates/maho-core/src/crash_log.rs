//! Port of senpi packages/coding-agent/src/core/crash-log.ts.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::source_info::{SourceInfo, SourceOrigin};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrashKind {
    UncaughtException,
    FatalError,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrashRecord {
    pub timestamp: String,
    pub version: String,
    pub kind: CrashKind,
    pub message: String,
    pub stack: Option<String>,
    pub session_file: Option<String>,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notified: Option<bool>,
}

const MAX_CRASH_RECORDS: usize = 5;
const MAX_AGE_MS: i64 = 7 * 24 * 60 * 60 * 1000;

pub fn crash_log_path(agent_dir: &str) -> String {
    Path::new(agent_dir)
        .join("crashes.json")
        .to_string_lossy()
        .into_owned()
}

/// Every well-formed record in the log; a missing or malformed log reads as empty.
pub fn read_crash_log(path: &str) -> Vec<CrashRecord> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(Value::Array(records)) = serde_json::from_str::<Value>(&content) else {
        return Vec::new();
    };
    records
        .into_iter()
        .filter(|record| {
            record.get("timestamp").is_some_and(Value::is_string)
                && record.get("message").is_some_and(Value::is_string)
        })
        .filter_map(|record| serde_json::from_value(record).ok())
        .collect()
}

fn write_crash_log(records: &[CrashRecord], path: &str) -> std::io::Result<()> {
    if let Some(parent) = Path::new(path).parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(records).map_err(std::io::Error::other)?;
    std::fs::write(path, format!("{text}\n"))
}

/// The extension metadata a stack is matched against (upstream `Pick<Extension, ...>`).
pub struct ExtensionStackMetadata<'a> {
    pub path: &'a str,
    pub resolved_path: &'a str,
    pub source_info: &'a SourceInfo,
}

fn is_synthetic_path(path: &str) -> bool {
    path.starts_with("builtin:") || path.starts_with('<')
}

fn normalize_stack_path(value: &str) -> String {
    value.replace('\\', "/").trim_end_matches('/').to_owned()
}

fn is_drive_path(target: &str) -> bool {
    let bytes = target.as_bytes();
    bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'/'
}

fn stack_contains_path(stack: &str, target_path: &str, include_descendants: bool) -> bool {
    let target = normalize_stack_path(target_path);
    if target.is_empty() || is_synthetic_path(&target) {
        return false;
    }
    let case_insensitive = is_drive_path(&target);
    let (haystack, needle) = if case_insensitive {
        (stack.to_lowercase(), target.to_lowercase())
    } else {
        (stack.to_owned(), target)
    };
    if include_descendants {
        return haystack.contains(&format!("{needle}/"));
    }
    haystack.match_indices(&needle).any(|(index, _)| {
        match haystack[index + needle.len()..].chars().next() {
            None => true,
            Some(next) => next == ':' || next == ')' || next.is_whitespace(),
        }
    })
}

fn is_remote_source(source: &str) -> bool {
    ["npm:", "git:", "http://", "https://", "ssh://"]
        .iter()
        .any(|prefix| source.starts_with(prefix))
}

fn is_script(path: &str) -> bool {
    [".js", ".ts", ".cjs", ".cts", ".mjs", ".mts"]
        .iter()
        .any(|suffix| path.ends_with(suffix))
}

/// Decodes `%XX` escapes like `decodeURI`; a malformed sequence leaves the line unchanged.
fn decode_uri(line: &str) -> String {
    let bytes = line.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = line
                .get(index + 1..index + 3)
                .and_then(|hex| u8::from_str_radix(hex, 16).ok());
            let Some(byte) = hex else {
                return line.to_owned();
            };
            out.push(byte);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| line.to_owned())
}

/// Find loaded extensions with source files in a stack trace.
pub fn find_extension_stack_matches(
    stack: Option<&str>,
    extensions: &[ExtensionStackMetadata<'_>],
) -> Vec<String> {
    let Some(stack) = stack.filter(|stack| !stack.is_empty()) else {
        return Vec::new();
    };
    let normalized_stack = stack
        .split('\n')
        .skip(1)
        .filter(|line| {
            let trimmed = line.trim_start();
            trimmed.len() < line.len()
                && trimmed.starts_with("at")
                && trimmed[2..].starts_with(char::is_whitespace)
        })
        .map(decode_uri)
        .collect::<Vec<_>>()
        .join("\n")
        .replace('\\', "/");
    let mut matches: Vec<String> = Vec::new();
    for extension in extensions {
        let resolved_path = normalize_stack_path(extension.resolved_path);
        let info = extension.source_info;
        let package = info.origin == SourceOrigin::Package;
        let single_file_package =
            package && !is_remote_source(&info.source) && is_script(&info.source);
        let package_root = info
            .base_dir
            .as_deref()
            .filter(|base| package && !single_file_package && !base.is_empty());
        let slash_index = resolved_path.rfind('/');
        let directory_entry = slash_index.is_some_and(|slash| {
            resolved_path[slash + 1..]
                .strip_prefix("index")
                .is_some_and(is_script_suffix)
        });
        let matched = match (package_root, slash_index) {
            (Some(root), _) => stack_contains_path(&normalized_stack, root, true),
            (None, Some(slash)) if directory_entry => {
                stack_contains_path(&normalized_stack, &resolved_path[..slash], true)
            }
            _ => stack_contains_path(&normalized_stack, &resolved_path, false),
        };
        if !matched {
            continue;
        }
        let label = if package && !info.source.is_empty() {
            info.source.clone()
        } else {
            extension.path.to_owned()
        };
        if !matches.contains(&label) {
            matches.push(label);
        }
    }
    matches
}

fn is_script_suffix(suffix: &str) -> bool {
    matches!(suffix, ".js" | ".ts" | ".cjs" | ".cts" | ".mjs" | ".mts")
}

pub struct CrashInput<'a> {
    pub kind: CrashKind,
    pub message: &'a str,
    pub stack: Option<&'a str>,
    pub session_file: Option<&'a str>,
    pub cwd: &'a str,
}

/// Best-effort persistence for callers that are already crashing.
pub fn record_crash(crash: CrashInput<'_>, path: &str) -> Option<CrashRecord> {
    let record = CrashRecord {
        timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        version: crate::engine_build_identity::ENGINE_VERSION.to_owned(),
        kind: crash.kind,
        message: crash.message.to_owned(),
        stack: crash
            .stack
            .filter(|stack| !stack.is_empty())
            .map(str::to_owned),
        session_file: crash.session_file.map(str::to_owned),
        cwd: crash.cwd.to_owned(),
        notified: None,
    };
    let mut records = read_crash_log(path);
    records.push(record.clone());
    let keep = records.len().saturating_sub(MAX_CRASH_RECORDS);
    write_crash_log(&records[keep..], path).ok()?;
    Some(record)
}

fn timestamp_ms(timestamp: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(timestamp)
        .ok()
        .map(|time| time.timestamp_millis())
}

/// Return the newest recent crash, marking pending records as announced.
pub fn take_unnotified_crash(path: &str, now_ms: i64) -> Option<CrashRecord> {
    let records = read_crash_log(path);
    let crash = records
        .iter()
        .rev()
        .find(|record| {
            record.notified != Some(true)
                && timestamp_ms(&record.timestamp).is_some_and(|time| now_ms - time <= MAX_AGE_MS)
        })?
        .clone();
    let marked: Vec<CrashRecord> = records
        .into_iter()
        .map(|record| {
            if record.notified == Some(true) {
                record
            } else {
                CrashRecord {
                    notified: Some(true),
                    ..record
                }
            }
        })
        .collect();
    // Showing the notice again is harmless.
    let _ = write_crash_log(&marked, path);
    Some(crash)
}

pub fn clear_crash_log(path: &str) {
    // The records can be attached again if cleanup fails.
    let _ = std::fs::remove_file(path);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source_info::SourceScope;

    fn info(origin: SourceOrigin, source: &str, base_dir: Option<&str>) -> SourceInfo {
        SourceInfo {
            path: String::new(),
            source: source.to_owned(),
            scope: SourceScope::User,
            origin,
            base_dir: base_dir.map(str::to_owned),
        }
    }

    #[test]
    fn records_keeps_the_last_five_and_announces_once() {
        let Some(dir) = tempfile::tempdir().ok() else {
            panic!("tempdir")
        };
        let path = crash_log_path(&dir.path().to_string_lossy());
        for index in 0..7 {
            let message = format!("boom {index}");
            let input = CrashInput {
                kind: CrashKind::FatalError,
                message: &message,
                stack: None,
                session_file: None,
                cwd: "/w",
            };
            assert!(record_crash(input, &path).is_some());
        }
        let records = read_crash_log(&path);
        assert_eq!(records.len(), 5);
        assert_eq!(records[0].message, "boom 2");
        let now = chrono::Utc::now().timestamp_millis();
        assert_eq!(
            take_unnotified_crash(&path, now)
                .map(|c| c.message)
                .as_deref(),
            Some("boom 6")
        );
        assert_eq!(take_unnotified_crash(&path, now), None);
        assert_eq!(
            take_unnotified_crash(&path, now + MAX_AGE_MS + 60_000),
            None
        );
        clear_crash_log(&path);
        assert!(read_crash_log(&path).is_empty());
    }

    #[test]
    fn matches_extension_files_and_package_roots_in_a_stack() {
        let stack = "Error: boom\n    at run (/home/u/.ext/tool.ts:3:9)\n    at x (/pkgs/acme/lib/a.js:1:1)\n    at y (/home/u/.ext/toolbox.ts:1:1)";
        let local = info(SourceOrigin::TopLevel, "", None);
        let package = info(SourceOrigin::Package, "npm:acme", Some("/pkgs/acme"));
        let missing = info(SourceOrigin::TopLevel, "", None);
        let extensions = [
            ExtensionStackMetadata {
                path: "tool",
                resolved_path: "/home/u/.ext/tool.ts",
                source_info: &local,
            },
            ExtensionStackMetadata {
                path: "acme",
                resolved_path: "/pkgs/acme/index.js",
                source_info: &package,
            },
            ExtensionStackMetadata {
                path: "other",
                resolved_path: "/home/u/.ext/too",
                source_info: &missing,
            },
        ];
        assert_eq!(
            find_extension_stack_matches(Some(stack), &extensions),
            vec!["tool".to_owned(), "npm:acme".to_owned()]
        );
        assert!(find_extension_stack_matches(None, &extensions).is_empty());
    }
}
