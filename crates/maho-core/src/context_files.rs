//! Port of senpi packages/coding-agent/src/core/context-files.ts (size limits).
//!
//! Discovery (`loadContextFileFromDir`, worktree shadowing, the ancestor walk) is ported in
//! `resource_loader::load_project_context_files`, owned by the senpi-core-rest lane. This module adds
//! the bound on what context files add to every request: budget is given nearest-first, oversized
//! files are cut at a line (closing an open code fence), and every cut or skipped file is named.

use serde::{Deserialize, Serialize};

use crate::diagnostics::{ResourceDiagnostic, ResourceDiagnosticType};
use crate::system_prompt::ContextFile;

pub const DEFAULT_CONTEXT_FILE_MAX_BYTES: usize = 64 * 1024;
pub const DEFAULT_CONTEXT_FILES_MAX_TOTAL_BYTES: usize = 128 * 1024;
const MIN_USEFUL_BYTES: usize = 1024;

/// Size limits for AGENTS.md / CLAUDE.md context files (`contextFiles.*` settings).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ContextFileLimits {
    pub max_bytes_per_file: Option<f64>,
    pub max_total_bytes: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextFiles {
    pub files: Vec<ContextFile>,
    pub diagnostics: Vec<ResourceDiagnostic>,
}

impl std::ops::Deref for ContextFiles {
    type Target = [ContextFile];
    fn deref(&self) -> &Self::Target {
        &self.files
    }
}

impl std::ops::DerefMut for ContextFiles {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.files
    }
}

impl IntoIterator for ContextFiles {
    type Item = ContextFile;
    type IntoIter = std::vec::IntoIter<ContextFile>;
    fn into_iter(self) -> Self::IntoIter {
        self.files.into_iter()
    }
}

impl AsRef<[ContextFile]> for ContextFiles {
    fn as_ref(&self) -> &[ContextFile] {
        &self.files
    }
}

fn kib(bytes: usize) -> String {
    let value = bytes as f64 / 1024.0;
    if value.fract() == 0.0 {
        format!("{value} KiB")
    } else {
        format!("{value:.1} KiB")
    }
}

/// The longest UTF-8 prefix within `max_bytes`, cut back to a line end when one lies in the last quarter.
fn utf8_prefix(content: &str, max_bytes: usize) -> &str {
    if max_bytes == 0 {
        return "";
    }
    let bytes = content.as_bytes();
    let mut end = max_bytes.min(bytes.len());
    while end > 0 && end < bytes.len() && !content.is_char_boundary(end) {
        end -= 1;
    }
    if let Some(newline) = bytes[..end].iter().rposition(|byte| *byte == b'\n')
        && newline as f64 >= max_bytes as f64 * 0.75
    {
        end = newline + 1;
    }
    &content[..end]
}

fn positive_limit(value: Option<f64>, fallback: usize) -> usize {
    match value {
        Some(value) if value.is_finite() && value > 0.0 => value.floor() as usize,
        _ => fallback,
    }
}

/// A fence opener/closer line: optional indent, then 3+ backticks or tildes, then the info string.
fn fence(line: &str) -> Option<(&str, &str)> {
    let trimmed = line.trim_start_matches([' ', '\t']);
    let marker_char = trimmed.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let length = trimmed.chars().take_while(|c| *c == marker_char).count();
    (length >= 3).then(|| trimmed.split_at(length))
}

fn open_fence(prefix: &str) -> Option<String> {
    let mut open: Option<String> = None;
    for line in prefix.split('\n') {
        let Some((marker, rest)) = fence(line) else {
            continue;
        };
        match &open {
            None => open = Some(marker.to_owned()),
            Some(opener)
                if marker.starts_with(&opener[..1])
                    && marker.len() >= opener.len()
                    && rest.trim().is_empty() =>
            {
                open = None;
            }
            Some(_) => {}
        }
    }
    open
}

fn size_warning(path: &str, message: String) -> ResourceDiagnostic {
    ResourceDiagnostic {
        diagnostic_type: ResourceDiagnosticType::Warning,
        category: Some("size-limit".to_owned()),
        message,
        path: Some(path.to_owned()),
        collision: None,
    }
}

/// Bound what the context files add to every request. Files are given budget nearest-first (the cwd's
/// file before its ancestors, the global file last), so the most specific instructions survive.
pub fn apply_context_file_limits(
    files: Vec<ContextFile>,
    limits: ContextFileLimits,
) -> ContextFiles {
    let per_file = positive_limit(limits.max_bytes_per_file, DEFAULT_CONTEXT_FILE_MAX_BYTES);
    let total = positive_limit(
        limits.max_total_bytes,
        DEFAULT_CONTEXT_FILES_MAX_TOTAL_BYTES,
    );
    let limit_text = format!(
        "limit {} per file, {} total; raise contextFiles.maxBytesPerFile / contextFiles.maxTotalBytes in settings to load more",
        kib(per_file),
        kib(total)
    );
    let mut kept: Vec<ContextFile> = Vec::new();
    let mut diagnostics = Vec::new();
    let mut remaining = total;
    for file in files.into_iter().rev() {
        let size = file.content.len();
        let allowed = per_file.min(remaining);
        if size <= allowed {
            remaining -= size;
            kept.insert(0, file);
            continue;
        }
        if allowed < MIN_USEFUL_BYTES {
            let reason = if per_file < MIN_USEFUL_BYTES {
                "the per-file limit is too small"
            } else {
                "nearer context files used the budget"
            };
            diagnostics.push(size_warning(
                &file.path,
                format!(
                    "Context file ({}) was not loaded: {reason} ({limit_text}).",
                    kib(size)
                ),
            ));
            continue;
        }
        let note = format!("\n\n[This file was truncated from {}.]", kib(size));
        let content_budget = allowed.saturating_sub(note.len());
        let mut prefix = utf8_prefix(&file.content, content_budget).to_owned();
        let content = loop {
            let closing = match open_fence(&prefix) {
                Some(marker) => format!(
                    "{}{marker}\n",
                    if prefix.ends_with('\n') { "" } else { "\n" }
                ),
                None => String::new(),
            };
            let content = format!("{prefix}{closing}{note}");
            if content.len() <= allowed {
                break content;
            }
            // A longer closing marker can expose another opener when the prefix shrinks.
            prefix = utf8_prefix(&prefix, content_budget.saturating_sub(closing.len())).to_owned();
        };
        diagnostics.push(size_warning(
            &file.path,
            format!(
                "Context file is {}; only the first {} was loaded ({limit_text}).",
                kib(size),
                kib(prefix.len())
            ),
        ));
        remaining -= content.len();
        kept.insert(
            0,
            ContextFile {
                path: file.path,
                content,
                scope: file.scope,
            },
        );
    }
    ContextFiles {
        files: kept,
        diagnostics,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, content: String) -> ContextFile {
        ContextFile {
            path: path.to_owned(),
            content,
            scope: None,
        }
    }

    /// Name the plan's IS-2 row cites. It runs the same assertions as
    /// `settings_context_files_limits_reach_the_loader`, which owns the body.
    #[test]
    fn context_file_limits_bound_the_live_loader() {
        settings_context_files_limits_reach_the_loader();
    }

    #[test]
    fn settings_context_files_limits_reach_the_loader() {
        let dir = crate::test_support::isolated_tempdir();
        let project = dir.path().join("project");
        let agent = dir.path().join("agent");
        std::fs::create_dir_all(&project).expect("project dir");
        std::fs::create_dir_all(&agent).expect("agent dir");
        std::fs::write(project.join("AGENTS.md"), "x\n".repeat(2048)).expect("context file");

        let settings_val = serde_json::json!({
            "maxBytesPerFile": 2048.0,
            "maxTotalBytes": 2048.0,
        });
        let limits: ContextFileLimits =
            serde_json::from_value(settings_val).expect("deserialize limits");

        let limited = crate::resource_loader::load_project_context_files(
            &project.to_string_lossy(),
            &agent.to_string_lossy(),
            Some(limits),
        );
        assert_eq!(limited.files.len(), 1);
        assert!(limited.files[0].content.len() <= 2048);
        assert!(
            limited.files[0]
                .content
                .contains("[This file was truncated from 4 KiB.]")
        );
        assert!(!limited.diagnostics.is_empty());
        assert_eq!(
            limited.diagnostics[0].category.as_deref(),
            Some("size-limit")
        );

        let unlimited = crate::resource_loader::load_project_context_files(
            &project.to_string_lossy(),
            &agent.to_string_lossy(),
            None,
        );
        assert_eq!(unlimited.files.len(), 1);
        assert_eq!(unlimited.files[0].content.len(), 4096);
        assert!(unlimited.diagnostics.is_empty());
    }

    #[test]
    fn context_file_scope_tagging_is_unchanged_by_limits() {
        let mut user = file("/agent/AGENTS.md", "u\n".repeat(1024));
        user.scope = Some(crate::system_prompt::ContextFileScope::User);
        let project = file("/project/AGENTS.md", "p\n".repeat(1024));

        let unlimited = apply_context_file_limits(
            vec![user.clone(), project.clone()],
            ContextFileLimits::default(),
        );
        let limits = ContextFileLimits {
            max_bytes_per_file: Some(1200.0),
            max_total_bytes: Some(2400.0),
        };
        let limited = apply_context_file_limits(vec![user, project], limits);

        assert_eq!(limited.files.len(), 2);
        assert_eq!(unlimited.files.len(), 2);
        assert_eq!(
            limited.files[0].scope,
            Some(crate::system_prompt::ContextFileScope::User)
        );
        assert_eq!(limited.files[0].scope, unlimited.files[0].scope);
        assert_eq!(limited.files[1].scope, None);
        assert_eq!(limited.files[1].scope, unlimited.files[1].scope);
        assert!(limited.files[0].content.len() <= 1200);
        assert!(limited.files[1].content.len() <= 1200);
        assert!(
            limited.files[0]
                .content
                .contains("[This file was truncated from 2 KiB.]")
        );
        assert!(
            limited.files[1]
                .content
                .contains("[This file was truncated from 2 KiB.]")
        );
    }

    #[test]
    fn small_files_load_unchanged() {
        let files = vec![
            file("/g/AGENTS.md", "global".into()),
            file("/p/AGENTS.md", "project".into()),
        ];
        let limited = apply_context_file_limits(files.clone(), ContextFileLimits::default());
        assert_eq!(limited.files, files);
        assert!(limited.diagnostics.is_empty());
    }

    #[test]
    fn the_nearest_file_wins_the_budget_and_the_cut_closes_a_fence() {
        let near = format!("```rust\n{}", "x\n".repeat(1500));
        let far = "y".repeat(2000);
        let limits = ContextFileLimits {
            max_bytes_per_file: Some(2048.0),
            max_total_bytes: Some(2500.0),
        };
        let limited =
            apply_context_file_limits(vec![file("/far", far), file("/near", near)], limits);
        assert_eq!(limited.files.len(), 1);
        let kept = &limited.files[0];
        assert_eq!(kept.path, "/near");
        assert!(kept.content.len() <= 2048);
        assert!(
            kept.content
                .contains("\n```\n\n\n[This file was truncated from 2.9 KiB.]")
        );
        assert_eq!(limited.diagnostics.len(), 2);
        assert!(
            limited.diagnostics[1]
                .message
                .contains("nearer context files used the budget")
        );
    }

    #[test]
    fn helpers_cut_at_char_and_line_boundaries() {
        assert_eq!(utf8_prefix("aé", 2), "a");
        assert_eq!(utf8_prefix("abcdef\ngh", 8), "abcdef\n");
        assert_eq!(kib(1536), "1.5 KiB");
        assert_eq!(kib(2048), "2 KiB");
        assert_eq!(positive_limit(Some(-1.0), 7), 7);
    }
}
