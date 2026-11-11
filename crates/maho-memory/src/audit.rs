//! Structural diagnostics over a memory corpus.
//!
//! Spec: `docs/cleanroom/specs/memory-audit.md`. Answers one question: is anything in here broken in a way a
//! reader would not notice on their own? Duplicate bodies, dangling links, invalid frontmatter, and files
//! outside the known homes are all silent — the note still renders.
//!
//! Takes the corpus as a list of `(path, bytes)` rather than walking a directory, so the rules are testable
//! without a filesystem and the caller owns symlink policy.

use std::collections::{BTreeMap, BTreeSet};

use crate::broken_links::dangling_links;
use crate::frontmatter::split_frontmatter;
use crate::paths::is_memory_content_path;
use crate::store::sha256_hex;

pub const CONTENT_HOMES: &[&str] = &["system", "reference", "notes", "people", "skills"];

const ROOT_FILES: &[&str] = &["ARCHIVE.md", "README.md"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryAuditIssue {
    pub code: String,
    pub path: String,
    pub detail: String,
    pub related: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryAuditReport {
    pub version: u8,
    pub issues: Vec<MemoryAuditIssue>,
    pub counts: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemoryAuditOptions {
    pub system_tokens: Option<usize>,
    pub budget_tokens: Option<usize>,
}

/// The share of the budget at which the system prompt is reported.
///
/// At 80% the next addition is likely to push it over, and the person can decide what to drop rather than
/// discovering the overflow when a block silently stops fitting.
pub const SYSTEM_PRESSURE_PERCENT: f64 = 0.8;

const CODES: [&str; 6] = [
    "link_dangling",
    "frontmatter_invalid",
    "content_duplicate",
    "path_orphan",
    "file_unreadable",
    "system_pressure",
];

fn issue(code: &str, path: &str, detail: impl Into<String>) -> MemoryAuditIssue {
    MemoryAuditIssue {
        code: code.to_owned(),
        path: path.to_owned(),
        detail: detail.into(),
        related: None,
    }
}

/// Audit a corpus given as repository-relative path and raw bytes.
///
/// Ordering is stable: files are sorted, so the same corpus produces the same report twice.
pub fn audit_files(files: &[(String, Vec<u8>)], options: &MemoryAuditOptions) -> MemoryAuditReport {
    let mut sorted: Vec<&(String, Vec<u8>)> = files.iter().collect();
    sorted.sort_by(|left, right| left.0.cmp(&right.0));
    let existing: BTreeSet<String> = sorted.iter().map(|(path, _)| path.clone()).collect();

    let mut issues = Vec::new();
    let mut bodies: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for (path, bytes) in &sorted {
        if !path.ends_with(".md") {
            continue;
        }
        let home = path
            .strip_prefix("memory/")
            .unwrap_or(path)
            .split('/')
            .next()
            .unwrap_or("");
        let orphan = !CONTENT_HOMES.contains(&home) && !ROOT_FILES.contains(&path.as_str());
        let Ok(content) = String::from_utf8(bytes.clone()) else {
            if orphan {
                issues.push(issue("path_orphan", path, "outside memory homes"));
            }
            issues.push(issue("file_unreadable", path, "invalid UTF-8"));
            continue;
        };
        let split = split_frontmatter(&content);
        if orphan {
            let detail = if split.is_some() {
                "outside memory homes"
            } else {
                "outside memory homes; no frontmatter (add a description in the same commit as the move, or leave it)"
            };
            issues.push(issue("path_orphan", path, detail));
        }
        if is_memory_content_path(path)
            && let Some(detail) = crate::frontmatter::describe_frontmatter_violation(&content)
        {
            issues.push(issue("frontmatter_invalid", path, detail));
        }
        let body = split.map(|(_, body)| body).unwrap_or(content);
        if !body.trim().is_empty() {
            bodies
                .entry(sha256_hex(&body))
                .or_default()
                .push(path.clone());
        }
        for dangling in dangling_links(path, &body, &existing) {
            issues.push(issue(
                "link_dangling",
                path,
                if dangling.escapes {
                    format!("{} (escapes repository)", dangling.target)
                } else {
                    dangling.target
                },
            ));
        }
    }

    for paths in bodies.values().filter(|paths| paths.len() > 1) {
        for path in paths.iter().skip(1) {
            let mut entry = issue(
                "content_duplicate",
                path,
                format!("same body as {}", paths[0]),
            );
            entry.related = Some(paths.clone());
            issues.push(entry);
        }
    }

    if let (Some(tokens), Some(budget)) = (options.system_tokens, options.budget_tokens)
        && budget > 0
        && tokens as f64 >= SYSTEM_PRESSURE_PERCENT * budget as f64
    {
        issues.push(issue(
            "system_pressure",
            "system/",
            format!("{tokens} tokens of {budget} budget"),
        ));
    }

    report(issues)
}

fn report(issues: Vec<MemoryAuditIssue>) -> MemoryAuditReport {
    let mut counts: BTreeMap<String, usize> = CODES
        .iter()
        .map(|code| ((*code).to_owned(), 0usize))
        .collect();
    for entry in &issues {
        *counts.entry(entry.code.clone()).or_default() += 1;
    }
    MemoryAuditReport {
        version: 1,
        issues,
        counts,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn corpus(files: &[(&str, &str)]) -> Vec<(String, Vec<u8>)> {
        files
            .iter()
            .map(|(path, content)| ((*path).to_owned(), content.as_bytes().to_vec()))
            .collect()
    }

    const VALID: &str = "---\ndescription: d\n---\nbody\n";

    #[test]
    fn a_clean_corpus_reports_nothing() {
        let report = audit_files(
            &corpus(&[
                ("system/persona.md", "---\ndescription: a\n---\none"),
                ("notes/a.md", "---\ndescription: b\n---\ntwo"),
                ("people/x/card.md", "---\ndescription: c\n---\nthree"),
            ]),
            &MemoryAuditOptions::default(),
        );
        assert!(report.issues.is_empty(), "{:?}", report.issues);
        assert_eq!(report.version, 1);
    }

    #[test]
    fn every_code_appears_in_the_counts_even_at_zero() {
        let report = audit_files(&[], &MemoryAuditOptions::default());
        for code in CODES {
            assert_eq!(report.counts.get(code), Some(&0), "{code}");
        }
    }

    #[test]
    fn an_orphan_path_is_reported() {
        let report = audit_files(
            &corpus(&[("stray/note.md", VALID)]),
            &MemoryAuditOptions::default(),
        );
        assert_eq!(report.counts["path_orphan"], 1);
    }

    #[test]
    fn an_orphan_without_frontmatter_says_so() {
        let report = audit_files(
            &corpus(&[("stray/note.md", "just text")]),
            &MemoryAuditOptions::default(),
        );
        assert!(
            report.issues[0].detail.contains("no frontmatter"),
            "{:?}",
            report.issues[0]
        );
    }

    #[test]
    fn a_root_readme_is_not_an_orphan() {
        let report = audit_files(
            &corpus(&[("README.md", VALID), ("ARCHIVE.md", VALID)]),
            &MemoryAuditOptions::default(),
        );
        assert_eq!(report.counts["path_orphan"], 0);
    }

    #[test]
    fn the_memory_prefix_is_accepted_on_a_home() {
        let report = audit_files(
            &corpus(&[("memory/system/persona.md", VALID)]),
            &MemoryAuditOptions::default(),
        );
        assert_eq!(report.counts["path_orphan"], 0);
    }

    #[test]
    fn a_non_markdown_file_is_not_audited() {
        let report = audit_files(
            &corpus(&[("stray/note.txt", "x"), ("notes/a.md", VALID)]),
            &MemoryAuditOptions::default(),
        );
        assert!(report.issues.is_empty(), "{:?}", report.issues);
    }

    #[test]
    fn invalid_frontmatter_is_reported() {
        let report = audit_files(
            &corpus(&[(
                "system/a.md",
                "---\ndescription: d\ndescription: again\n---\nbody",
            )]),
            &MemoryAuditOptions::default(),
        );
        assert_eq!(report.counts["frontmatter_invalid"], 1);
    }

    #[test]
    fn invalid_utf8_is_unreadable() {
        let report = audit_files(
            &[("notes/a.md".to_owned(), vec![0xff, 0xfe])],
            &MemoryAuditOptions::default(),
        );
        assert_eq!(report.counts["file_unreadable"], 1);
    }

    #[test]
    fn an_unreadable_orphan_keeps_both_diagnostics() {
        let report = audit_files(
            &[("stray/a.md".to_owned(), vec![0xff, 0xfe])],
            &MemoryAuditOptions::default(),
        );
        assert_eq!(report.counts["file_unreadable"], 1);
        assert_eq!(report.counts["path_orphan"], 1);
    }

    #[test]
    fn duplicate_bodies_are_reported_on_the_later_path_only() {
        let report = audit_files(
            &corpus(&[
                ("notes/a.md", "---\ndescription: one\n---\nsame body"),
                ("notes/b.md", "---\ndescription: two\n---\nsame body"),
            ]),
            &MemoryAuditOptions::default(),
        );
        assert_eq!(report.counts["content_duplicate"], 1);
        assert_eq!(report.issues[0].path, "notes/b.md");
        assert_eq!(
            report.issues[0].related,
            Some(vec!["notes/a.md".to_owned(), "notes/b.md".to_owned()])
        );
    }

    #[test]
    fn identical_bodies_are_compared_without_frontmatter() {
        let report = audit_files(
            &corpus(&[
                ("notes/a.md", "---\ndescription: one\n---\nsame"),
                ("notes/b.md", "---\ndescription: two\n---\nsame"),
            ]),
            &MemoryAuditOptions::default(),
        );
        assert_eq!(report.counts["content_duplicate"], 1);
    }

    #[test]
    fn an_empty_body_is_not_a_duplicate() {
        let report = audit_files(
            &corpus(&[
                ("notes/a.md", "---\ndescription: one\n---\n"),
                ("notes/b.md", "---\ndescription: two\n---\n"),
            ]),
            &MemoryAuditOptions::default(),
        );
        assert_eq!(report.counts["content_duplicate"], 0);
    }

    #[test]
    fn three_copies_report_two_duplicates() {
        let report = audit_files(
            &corpus(&[
                ("notes/a.md", "---\ndescription: 1\n---\nx"),
                ("notes/b.md", "---\ndescription: 2\n---\nx"),
                ("notes/c.md", "---\ndescription: 3\n---\nx"),
            ]),
            &MemoryAuditOptions::default(),
        );
        assert_eq!(report.counts["content_duplicate"], 2);
    }

    #[test]
    fn a_dangling_link_is_reported() {
        let report = audit_files(
            &corpus(&[("notes/a.md", "---\ndescription: d\n---\nsee [[notes/gone]]")]),
            &MemoryAuditOptions::default(),
        );
        assert_eq!(report.counts["link_dangling"], 1);
    }

    #[test]
    fn a_resolving_link_is_not_reported() {
        let report = audit_files(
            &corpus(&[
                ("notes/a.md", "---\ndescription: d\n---\nsee [[notes/b]]"),
                ("notes/b.md", VALID),
            ]),
            &MemoryAuditOptions::default(),
        );
        assert_eq!(report.counts["link_dangling"], 0);
    }

    #[test]
    fn an_escaping_link_says_so() {
        let report = audit_files(
            &corpus(&[(
                "notes/a.md",
                "---\ndescription: d\n---\n[out](../../outside.md)",
            )]),
            &MemoryAuditOptions::default(),
        );
        assert!(
            report.issues[0].detail.contains("escapes repository"),
            "{:?}",
            report.issues[0]
        );
    }

    #[test]
    fn system_pressure_is_reported_at_four_fifths_of_the_budget() {
        let report = audit_files(
            &corpus(&[("system/a.md", VALID)]),
            &MemoryAuditOptions {
                system_tokens: Some(800),
                budget_tokens: Some(1000),
            },
        );
        assert_eq!(report.counts["system_pressure"], 1);
        assert!(report.issues[0].detail.contains("800 tokens of 1000"));
    }

    #[test]
    fn system_pressure_is_not_reported_below_the_threshold() {
        let report = audit_files(
            &corpus(&[("system/a.md", VALID)]),
            &MemoryAuditOptions {
                system_tokens: Some(799),
                budget_tokens: Some(1000),
            },
        );
        assert_eq!(report.counts["system_pressure"], 0);
    }

    #[test]
    fn pressure_needs_both_numbers() {
        let report = audit_files(
            &corpus(&[("system/a.md", VALID)]),
            &MemoryAuditOptions {
                system_tokens: Some(900),
                budget_tokens: None,
            },
        );
        assert_eq!(report.counts["system_pressure"], 0);
    }

    #[test]
    fn a_zero_budget_never_divides_by_zero() {
        let report = audit_files(
            &corpus(&[("system/a.md", VALID)]),
            &MemoryAuditOptions {
                system_tokens: Some(1),
                budget_tokens: Some(0),
            },
        );
        assert_eq!(report.counts["system_pressure"], 0);
    }

    #[test]
    fn the_report_is_stable_across_input_order() {
        let forward = audit_files(
            &corpus(&[
                ("notes/a.md", "---\ndescription: 1\n---\nx"),
                ("notes/b.md", "---\ndescription: 2\n---\nx"),
            ]),
            &MemoryAuditOptions::default(),
        );
        let backward = audit_files(
            &corpus(&[
                ("notes/b.md", "---\ndescription: 2\n---\nx"),
                ("notes/a.md", "---\ndescription: 1\n---\nx"),
            ]),
            &MemoryAuditOptions::default(),
        );
        assert_eq!(forward.issues, backward.issues);
    }
}
