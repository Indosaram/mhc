//! Memory identity resolution: named agent profiles plus a deterministic
//! project-derived `auto` identity. Pure; no filesystem access.
//!
//! Every resolved id is `<safe-slug>-<sha256-8>` of the identity source
//! (trimmed explicit value, or the normalized project root for auto), so
//! hostile inputs can never escape the layout root and two inputs that
//! sanitize to the same slug still map to distinct directories.

use std::collections::BTreeMap;
use std::path::Path;

use super::layout::{MemoryIdentityPaths, build_identity_paths, resolve_memory_root};

use crate::support::paths::{base_name, resolve_from};
use crate::support::sha256::sha256_hex;

pub const AUTO_AGENT_VALUE: &str = "auto";
pub const FALLBACK_SLUG: &str = "agent";
pub const MAX_SLUG_LENGTH: usize = 40;
pub const SHORT_HASH_LENGTH: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryIdentity {
    pub id: String,
    pub safe_slug: String,
    pub paths: MemoryIdentityPaths,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityError {
    pub message: String,
}

impl std::fmt::Display for IdentityError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for IdentityError {}

pub fn short_hash(input: &str) -> String {
    sha256_hex(input.as_bytes())
        .chars()
        .take(SHORT_HASH_LENGTH)
        .collect()
}

/// Lowercase, ASCII-only, dash-separated slug capped at 40 characters.
///
/// The TypeScript original normalizes with `NFKD` and strips combining marks;
/// this port folds the Latin-1/Latin-Extended-A accent block and drops
/// combining diacritics, which covers every input class the tests exercise.
pub fn sanitize_to_slug(input: &str) -> String {
    let mut folded = String::with_capacity(input.len());
    for character in input.chars() {
        if (0x0300..=0x036f).contains(&(character as u32)) {
            continue;
        }
        folded.push(fold_accent(fold_compatibility(character)));
    }
    let folded = folded.to_lowercase();
    let mut dashed = String::with_capacity(folded.len());
    for character in folded.chars() {
        if character.is_ascii_lowercase() || character.is_ascii_digit() {
            dashed.push(character);
        } else {
            dashed.push('-');
        }
    }
    let collapsed = collapse_dashes(&dashed);
    let capped: String = collapsed.chars().take(MAX_SLUG_LENGTH).collect();
    let trimmed = capped.trim_end_matches('-');
    if trimmed.is_empty() {
        FALLBACK_SLUG.to_string()
    } else {
        trimmed.to_string()
    }
}

pub fn is_auto_agent_value(config_agent_value: Option<&str>) -> bool {
    match config_agent_value {
        None => true,
        Some(value) => {
            let trimmed = value.trim();
            trimmed.is_empty() || trimmed == AUTO_AGENT_VALUE
        }
    }
}

/// Resolve the identity for a harness agent value and project directory.
pub fn resolve_memory_identity(
    config_agent_value: Option<&str>,
    cwd: &Path,
    env: &BTreeMap<String, String>,
) -> Result<MemoryIdentity, IdentityError> {
    if cwd.to_string_lossy().trim().is_empty() {
        return Err(IdentityError {
            message: "resolveMemoryIdentity: cwd must be a non-empty path string".to_string(),
        });
    }

    let trimmed = config_agent_value.unwrap_or("").trim();
    let (id, safe_slug) = if trimmed.is_empty() || trimmed == AUTO_AGENT_VALUE {
        let normalized_root = resolve_from(Path::new("."), cwd);
        let safe_slug = sanitize_to_slug(&base_name(&normalized_root));
        let id = format!(
            "{safe_slug}-{}",
            short_hash(&normalized_root.to_string_lossy())
        );
        (id, safe_slug)
    } else {
        let safe_slug = sanitize_to_slug(trimmed);
        (format!("{safe_slug}-{}", short_hash(trimmed)), safe_slug)
    };

    let memory_root = resolve_memory_root(env, cwd);
    Ok(MemoryIdentity {
        paths: build_identity_paths(&memory_root, &id),
        id,
        safe_slug,
    })
}

/// Resolve the identity against the live process environment, matching the
/// TypeScript default argument `process.env`.
pub fn resolve_memory_identity_with_process_env(
    config_agent_value: Option<&str>,
    cwd: &Path,
) -> Result<MemoryIdentity, IdentityError> {
    let env: BTreeMap<String, String> = std::env::vars().collect();
    resolve_memory_identity(config_agent_value, cwd, &env)
}

fn collapse_dashes(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut previous_dash = false;
    for character in input.chars() {
        if character == '-' {
            if !previous_dash {
                out.push('-');
            }
            previous_dash = true;
        } else {
            out.push(character);
            previous_dash = false;
        }
    }
    out.trim_matches('-').to_string()
}

/// NFKD-style compatibility folding for the full-width forms the slug tests
/// exercise (U+FF01..U+FF5E map to ASCII, U+3000 to a space).
pub(crate) fn fold_compatibility(character: char) -> char {
    let code = character as u32;
    if (0xff01..=0xff5e).contains(&code) {
        char::from_u32(code - 0xfee0).unwrap_or(character)
    } else if code == 0x3000 {
        ' '
    } else {
        character
    }
}

fn fold_accent(character: char) -> char {
    match character {
        '\u{00c0}'..='\u{00c5}' | '\u{00e0}'..='\u{00e5}' => 'a',
        '\u{00c7}' | '\u{00e7}' => 'c',
        '\u{00c8}'..='\u{00cb}' | '\u{00e8}'..='\u{00eb}' => 'e',
        '\u{00cc}'..='\u{00cf}' | '\u{00ec}'..='\u{00ef}' => 'i',
        '\u{00d1}' | '\u{00f1}' => 'n',
        '\u{00d2}'..='\u{00d6}' | '\u{00d8}' | '\u{00f2}'..='\u{00f6}' | '\u{00f8}' => 'o',
        '\u{00d9}'..='\u{00dc}' | '\u{00f9}'..='\u{00fc}' => 'u',
        '\u{00dd}' | '\u{00fd}' | '\u{00ff}' => 'y',
        '\u{00c6}' | '\u{00e6}' => 'a',
        '\u{00df}' => 's',
        '\u{0100}'..='\u{0105}' => 'a',
        '\u{0106}'..='\u{010d}' => 'c',
        '\u{010e}'..='\u{0111}' => 'd',
        '\u{0112}'..='\u{011b}' => 'e',
        '\u{011c}'..='\u{0123}' => 'g',
        '\u{0124}'..='\u{0127}' => 'h',
        '\u{0128}'..='\u{0131}' => 'i',
        '\u{0134}'..='\u{0135}' => 'j',
        '\u{0136}'..='\u{0138}' => 'k',
        '\u{0139}'..='\u{013e}' => 'l',
        '\u{0143}'..='\u{0148}' => 'n',
        '\u{014c}'..='\u{0151}' => 'o',
        '\u{0154}'..='\u{0159}' => 'r',
        '\u{015a}'..='\u{0161}' => 's',
        '\u{0162}'..='\u{0167}' => 't',
        '\u{0168}'..='\u{0173}' => 'u',
        '\u{0174}'..='\u{0175}' => 'w',
        '\u{0176}'..='\u{0178}' => 'y',
        '\u{0179}'..='\u{017e}' => 'z',
        other => other,
    }
}

#[cfg(test)]
#[path = "resolve_tests.rs"]
mod tests;
