//! `team/import-discipline.test.ts`

use pretty_assertions::assert_eq;
use std::fs;
use std::path::{Path, PathBuf};

/// Allowed `team_core` submodules (Rust names of the TS exported subpaths). `error` is included
/// because the Rust team layer uses `team_core::Result` / `team_core::TeamCoreError`.
const ALLOWED_TEAM_CORE_MODULES: [&str; 7] = [
    "team_registry",
    "team_mailbox",
    "team_tasklist",
    "team_state_store",
    "types",
    "config",
    "error",
];

/// Crate-root re-exports of `team_core` that map onto allowed modules (`config`, `error`).
const ALLOWED_TEAM_CORE_ROOT_REEXPORTS: [&str; 3] = ["Result", "TeamCoreError", "TeamModeConfig"];

/// Forbidden `team_core` path prefixes (segment-wise).
const FORBIDDEN_TEAM_CORE_PREFIXES: [&str; 4] = [
    "team_core::team_worktree",
    "team_core::team_layout_tmux",
    "team_core::team_state_store::session_liveness",
    "team_core::team_mailbox::pending_delivery_recovery",
];

/// Forbidden crate roots anywhere in a path.
const FORBIDDEN_CRATES: [&str; 3] = ["tmux_core", "maho_tmux_core", "omo_opencode"];

fn team_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join("team")
}

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(dir).expect("read team dir");
    for entry in entries {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out);
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        // Rust test files carry the forbidden names as string literals; scan sources only.
        if name.ends_with(".rs") && !name.ends_with("_tests.rs") {
            out.push(path);
        }
    }
}

fn list_team_source_files() -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_rs_files(&team_dir(), &mut files);
    files.sort();
    files
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

/// Replaces comments, string literals and char literals with spaces so only code is scanned.
fn strip_comments_and_strings(source: &str) -> String {
    let chars: Vec<char> = source.chars().collect();
    let len = chars.len();
    let at = |i: usize| -> Option<char> { chars.get(i).copied() };
    let mut out = String::with_capacity(source.len());
    let mut i = 0;
    while i < len {
        let c = chars[i];
        let next = at(i + 1);
        let prev = if i > 0 { at(i - 1) } else { None };
        if c == '/' && next == Some('/') {
            while i < len && chars[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && next == Some('*') {
            let mut depth = 1;
            i += 2;
            while i < len && depth > 0 {
                if chars[i] == '/' && at(i + 1) == Some('*') {
                    depth += 1;
                    i += 2;
                } else if chars[i] == '*' && at(i + 1) == Some('/') {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            out.push(' ');
        } else if c == 'r'
            && matches!(next, Some('"') | Some('#'))
            && !prev.is_some_and(is_ident_char)
        {
            let mut j = i + 1;
            let mut hashes = 0;
            while at(j) == Some('#') {
                hashes += 1;
                j += 1;
            }
            if at(j) == Some('"') {
                j += 1;
                'raw: while j < len {
                    if chars[j] == '"' {
                        let mut k = 0;
                        while k < hashes && at(j + 1 + k) == Some('#') {
                            k += 1;
                        }
                        if k == hashes {
                            j += 1 + hashes;
                            break 'raw;
                        }
                    }
                    j += 1;
                }
                i = j;
                out.push(' ');
            } else {
                out.push(c);
                i += 1;
            }
        } else if c == '"' {
            i += 1;
            while i < len {
                if chars[i] == '\\' {
                    i += 2;
                    continue;
                }
                if chars[i] == '"' {
                    i += 1;
                    break;
                }
                i += 1;
            }
            out.push(' ');
        } else if c == '\'' {
            if next == Some('\\') {
                let mut j = i + 2;
                while j < len && chars[j] != '\'' {
                    j += 1;
                }
                i = j + 1;
                out.push(' ');
            } else if at(i + 2) == Some('\'') {
                i += 3;
                out.push(' ');
            } else {
                // lifetime
                out.push(c);
                i += 1;
            }
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

fn skip_ws(chars: &[char], pos: &mut usize) {
    while *pos < chars.len() && chars[*pos].is_whitespace() {
        *pos += 1;
    }
}

fn read_ident(chars: &[char], pos: &mut usize) -> String {
    let start = *pos;
    while *pos < chars.len() && is_ident_char(chars[*pos]) {
        *pos += 1;
    }
    chars[start..*pos].iter().collect()
}

/// Parses a (possibly grouped) path tree following `prefix::` and pushes every expanded path.
fn parse_tree(chars: &[char], pos: &mut usize, prefix: &str, out: &mut Vec<String>) {
    skip_ws(chars, pos);
    match chars.get(*pos).copied() {
        Some('{') => {
            *pos += 1;
            loop {
                skip_ws(chars, pos);
                match chars.get(*pos).copied() {
                    None => break,
                    Some('}') => {
                        *pos += 1;
                        break;
                    }
                    _ => {}
                }
                let before = *pos;
                parse_tree(chars, pos, prefix, out);
                skip_ws(chars, pos);
                // optional `as alias`
                if chars.get(*pos) == Some(&'a')
                    && chars.get(*pos + 1) == Some(&'s')
                    && !chars.get(*pos + 2).copied().is_some_and(is_ident_char)
                {
                    *pos += 2;
                    skip_ws(chars, pos);
                    read_ident(chars, pos);
                    skip_ws(chars, pos);
                }
                match chars.get(*pos).copied() {
                    Some(',') => *pos += 1,
                    Some('}') => {
                        *pos += 1;
                        break;
                    }
                    _ => {
                        if *pos == before {
                            // malformed; avoid infinite loop
                            break;
                        }
                        break;
                    }
                }
            }
        }
        Some('*') => {
            *pos += 1;
            out.push(format!("{prefix}::*"));
        }
        Some(c) if is_ident_start(c) => {
            let ident = read_ident(chars, pos);
            let path = format!("{prefix}::{ident}");
            let after_ident = *pos;
            skip_ws(chars, pos);
            if chars.get(*pos) == Some(&':') && chars.get(*pos + 1) == Some(&':') {
                let mut look = *pos + 2;
                skip_ws(chars, &mut look);
                match chars.get(look).copied() {
                    Some(n) if n == '{' || n == '*' || is_ident_start(n) => {
                        *pos = look;
                        parse_tree(chars, pos, &path, out);
                    }
                    _ => {
                        // turbofish or similar
                        *pos = after_ident;
                        out.push(path);
                    }
                }
            } else {
                *pos = after_ident;
                out.push(path);
            }
        }
        _ => out.push(prefix.to_string()),
    }
}

/// Extracts every fully expanded `team_core::...` path from stripped source.
fn extract_team_core_paths(stripped: &str) -> Vec<String> {
    let chars: Vec<char> = stripped.chars().collect();
    let needle: Vec<char> = "team_core::".chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i + needle.len() <= chars.len() {
        let preceded_by_ident = i > 0 && is_ident_char(chars[i - 1]);
        if !preceded_by_ident && chars[i..i + needle.len()] == needle[..] {
            let mut pos = i + needle.len();
            parse_tree(&chars, &mut pos, "team_core", &mut out);
            i = pos.max(i + 1);
        } else {
            i += 1;
        }
    }
    out
}

/// Extracts every path segment that is followed by `::`, plus `use`/`extern crate` roots.
fn extract_path_segments(stripped: &str) -> Vec<String> {
    let segment = regex::Regex::new(r"\b([A-Za-z_][A-Za-z0-9_]*)\s*::").expect("segment regex");
    let root = regex::Regex::new(r"\b(?:use|extern\s+crate)\s+([A-Za-z_][A-Za-z0-9_]*)")
        .expect("root regex");
    let mut out: Vec<String> = segment
        .captures_iter(stripped)
        .map(|c| c[1].to_string())
        .collect();
    out.extend(root.captures_iter(stripped).map(|c| c[1].to_string()));
    out
}

fn has_segment_prefix(path: &str, prefix: &str) -> bool {
    path == prefix || path.starts_with(&format!("{prefix}::"))
}

fn read_stripped(file: &Path) -> String {
    let source = fs::read_to_string(file).expect("read source file");
    strip_comments_and_strings(&source)
}

#[test]
fn given_the_team_layer_source_files_when_their_imports_are_scanned_then_no_forbidden_team_core_or_opencode_tmux_specifier_is_used()
 {
    // given
    let files = list_team_source_files();

    // when
    let mut violations: Vec<String> = Vec::new();
    for file in &files {
        let stripped = read_stripped(file);
        for path in extract_team_core_paths(&stripped) {
            let is_forbidden = FORBIDDEN_TEAM_CORE_PREFIXES
                .iter()
                .any(|prefix| has_segment_prefix(&path, prefix))
                || path.contains("team_mode");
            if is_forbidden {
                violations.push(format!("{}: {}", file.display(), path));
            }
        }
        for segment in extract_path_segments(&stripped) {
            let is_forbidden =
                FORBIDDEN_CRATES.contains(&segment.as_str()) || segment.contains("team_mode");
            if is_forbidden {
                violations.push(format!("{}: {}", file.display(), segment));
            }
        }
    }

    // then
    assert_eq!(violations, Vec::<String>::new());
}

#[test]
fn given_team_core_imports_when_scanned_then_only_exported_subpaths_are_used() {
    // given
    let files = list_team_source_files();

    // when
    let mut disallowed: Vec<String> = Vec::new();
    for file in &files {
        let stripped = read_stripped(file);
        for path in extract_team_core_paths(&stripped) {
            let first = path
                .strip_prefix("team_core::")
                .and_then(|rest| rest.split("::").next())
                .unwrap_or("");
            let allowed = ALLOWED_TEAM_CORE_MODULES.contains(&first)
                || ALLOWED_TEAM_CORE_ROOT_REEXPORTS.contains(&first);
            if !allowed {
                disallowed.push(format!("{}: {}", file.display(), path));
            }
        }
    }

    // then
    assert_eq!(disallowed, Vec::<String>::new());
}

#[test]
fn given_the_team_dir_when_listed_then_source_files_are_found_and_test_files_are_excluded() {
    let files = list_team_source_files();

    assert!(!files.is_empty(), "expected team source files under src/team");
    let test_files: Vec<String> = files
        .iter()
        .filter(|f| f.to_string_lossy().ends_with("_tests.rs"))
        .map(|f| f.display().to_string())
        .collect();
    assert_eq!(test_files, Vec::<String>::new());
}

#[test]
fn given_grouped_use_trees_when_expanded_then_forbidden_nested_paths_are_detected() {
    let source = "use team_core::{types::Message, team_mailbox::{pending_delivery_recovery::x, send}};\n\
                  // use team_core::team_worktree;\n\
                  let s = \"team_core::team_layout_tmux\";\n\
                  fn f() -> team_core::Result<()> { Ok(()) }\n";
    let stripped = strip_comments_and_strings(source);

    let paths = extract_team_core_paths(&stripped);

    assert_eq!(
        paths,
        vec![
            "team_core::types::Message".to_string(),
            "team_core::team_mailbox::pending_delivery_recovery::x".to_string(),
            "team_core::team_mailbox::send".to_string(),
            "team_core::Result".to_string(),
        ]
    );
}
