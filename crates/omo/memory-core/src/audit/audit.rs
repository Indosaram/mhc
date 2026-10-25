//! Structural memory corpus diagnostics, without following symlinks.
use crate::fs::resilient as fs;
use crate::memfs::{
    frontmatter_scalar::split_frontmatter, frontmatter_validation::describe_frontmatter_violation,
    paths::is_memory_content_path,
};
use crate::support::sha256::sha256_hex;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
#[derive(Debug, Clone, serde::Serialize)]
pub struct MemoryAuditIssue {
    pub code: String,
    pub path: String,
    pub detail: String,
    pub related: Option<Vec<String>>,
}
#[derive(Debug, serde::Serialize)]
pub struct MemoryAuditReport {
    pub version: u8,
    pub generated_at: String,
    pub issues: Vec<MemoryAuditIssue>,
    pub counts: BTreeMap<String, usize>,
}
#[derive(Debug, Default)]
pub struct MemoryAuditOptions {
    pub system_tokens: Option<usize>,
    pub budget_tokens: Option<usize>,
}
fn issue(code: &str, path: &str, detail: impl Into<String>) -> MemoryAuditIssue {
    MemoryAuditIssue {
        code: code.into(),
        path: path.into(),
        detail: detail.into(),
        related: None,
    }
}
pub fn audit_memory_repo(
    root: &Path,
    options: &MemoryAuditOptions,
) -> std::io::Result<MemoryAuditReport> {
    let mut files = Vec::new();
    list_files(root, "", &mut files)?;
    files.sort();
    let existing: BTreeSet<String> = files.iter().cloned().collect();
    let mut issues = Vec::new();
    let mut bodies: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for path in files.iter().filter(|p| p.ends_with(".md")) {
        let home = path
            .strip_prefix("memory/")
            .unwrap_or(path)
            .split('/')
            .next()
            .unwrap_or("");
        let orphan = !["system", "reference", "notes", "people", "skills"].contains(&home)
            && !["ARCHIVE.md", "README.md"].contains(&path.as_str());
        let content = match fs::read(&root.join(path)) {
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(text) => text,
                Err(_) => {
                    if orphan {
                        issues.push(issue("path_orphan", path, "outside memory homes"));
                    }
                    issues.push(issue("file_unreadable", path, "invalid UTF-8"));
                    continue;
                }
            },
            Err(_) => {
                if orphan {
                    issues.push(issue("path_orphan", path, "outside memory homes"));
                }
                issues.push(issue("file_unreadable", path, "cannot read file"));
                continue;
            }
        };
        let split = split_frontmatter(&content);
        if orphan {
            issues.push(issue("path_orphan", path, if split.is_some() { "outside memory homes" } else { "outside memory homes; no frontmatter (add a description in the same commit as the move, or leave it)" }));
        }
        if is_memory_content_path(path)
            && let Some(detail) = describe_frontmatter_violation(&content) {
                issues.push(issue("frontmatter_invalid", path, detail));
            }
        let body = split.map(|(_, body)| body).unwrap_or(content);
        if !body.trim().is_empty() {
            bodies
                .entry(sha256_hex(body.as_bytes()))
                .or_default()
                .push(path.clone());
        }
        issues.extend(dangling_links(path, &body, &existing));
    }
    for paths in bodies.values().filter(|p| p.len() > 1) {
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
        && tokens as f64 >= 0.8 * budget as f64 {
            issues.push(issue(
                "system_pressure",
                "system/",
                format!("{tokens} tokens of {budget} budget"),
            ));
        }
    let mut counts: BTreeMap<String, usize> = [
        "link_dangling",
        "frontmatter_invalid",
        "content_duplicate",
        "path_orphan",
        "file_unreadable",
        "system_pressure",
    ]
    .into_iter()
    .map(|s| (s.into(), 0))
    .collect();
    for entry in &issues {
        *counts.entry(entry.code.clone()).or_default() += 1;
    }
    Ok(MemoryAuditReport {
        version: 1,
        generated_at: timestamp(),
        issues,
        counts,
    })
}
fn timestamp() -> String {
    // Civil date conversion, UTC, matching Date.toISOString without a time dependency.
    let duration = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = duration.as_secs();
    let z = (secs / 86400) as i64 + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        secs / 3600 % 24,
        secs / 60 % 60,
        secs % 60,
        duration.subsec_millis()
    )
}
fn list_files(root: &Path, dir: &str, files: &mut Vec<String>) -> std::io::Result<()> {
    for name in fs::read_dir_names(&root.join(dir))? {
        if name == ".git" || name == ".tmp" {
            continue;
        }
        let relative = if dir.is_empty() {
            name
        } else {
            format!("{dir}/{name}")
        };
        let info = fs::symlink_metadata(&root.join(&relative))?;
        if info.is_symlink() {
            continue;
        }
        if info.is_dir() {
            list_files(root, &relative, files)?;
        } else if info.is_file() {
            files.push(relative);
        }
    }
    Ok(())
}
fn normalize(path: &str) -> String {
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." if parts.last().is_some_and(|s| *s != "..") => {
                parts.pop();
            }
            _ => parts.push(part),
        }
    }
    parts.join("/")
}
fn dangling_links(path: &str, body: &str, existing: &BTreeSet<String>) -> Vec<MemoryAuditIssue> {
    let mut issues = Vec::new();
    let mut fence: Option<(char, usize)> = None;
    for line in body.lines() {
        let trimmed = line.trim_start_matches(' ');
        let indent = line.len() - trimmed.len();
        let marker = trimmed.chars().next().filter(|c| *c == '`' || *c == '~');
        let run = marker.map_or(0, |c| trimmed.chars().take_while(|x| *x == c).count());
        if let Some((c, n)) = fence {
            if indent <= 3 && marker == Some(c) && run >= n && trimmed[run..].trim().is_empty() {
                fence = None;
            }
            continue;
        }
        if indent <= 3 && run >= 3 {
            fence = marker.map(|c| (c, run));
            continue;
        }
        let mut text = String::new();
        let mut rest = line;
        while let Some(i) = rest.find('`') {
            text.push_str(&rest[..i]);
            let n = rest[i..].bytes().take_while(|c| *c == b'`').count();
            let delim = "`".repeat(n);
            if let Some(end) = rest[i + n..].find(&delim) {
                rest = &rest[i + n + end + n..];
            } else {
                text.push_str(&rest[i..]);
                rest = "";
                break;
            }
        }
        text.push_str(rest);
        let mut targets = Vec::new();
        let mut rest = text.as_str();
        while let Some(i) = rest.find("[[") {
            rest = &rest[i + 2..];
            if let Some(end) = rest.find("]]") {
                targets.push((rest[..end].split('|').next().unwrap_or("").to_owned(), true));
                rest = &rest[end + 2..];
            } else {
                break;
            }
        }
        let mut rest = text.as_str();
        while let Some(i) = rest.find('[') {
            let image = i > 0 && rest.as_bytes()[i - 1] == b'!';
            rest = &rest[i + 1..];
            let Some(end) = rest.find("](") else {
                continue;
            };
            let after = &rest[end + 2..];
            let Some(close) = after.find(')') else {
                break;
            };
            if !image {
                let raw = after[..close].trim();
                let dest = if let Some(raw) = raw.strip_prefix('<') {
                    raw.split('>').next().unwrap_or(raw)
                } else {
                    raw.split_whitespace().next().unwrap_or("")
                };
                targets.push((dest.to_owned(), false));
            }
            rest = &after[close + 1..];
        }
        for (raw, wiki) in targets {
            let target = raw.split('#').next().unwrap_or("").trim();
            let lower = target.to_ascii_lowercase();
            if target.is_empty()
                || lower.starts_with("https:")
                || lower.starts_with("http:")
                || lower.starts_with("mailto:")
            {
                continue;
            }
            let joined = if wiki {
                target.to_owned()
            } else {
                format!(
                    "{}/{}",
                    path.rsplit_once('/').map_or("", |(p, _)| p),
                    target
                )
            };
            let resolved = normalize(&joined);
            let escapes =
                target.starts_with('/') || resolved == ".." || resolved.starts_with("../");
            if !escapes
                && (existing.contains(&resolved)
                    || wiki && existing.contains(&format!("{resolved}.md"))
                    || existing
                        .iter()
                        .any(|p| p.starts_with(&format!("{resolved}/"))))
            {
                continue;
            }
            issues.push(issue(
                "link_dangling",
                path,
                if escapes {
                    format!("{target} (escapes repository)")
                } else {
                    target.to_owned()
                },
            ));
        }
    }
    issues
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn orphan_warning_and_invalid_utf8_preserve_both_diagnostics() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(&dir.path().join("bare.md"), b"orphan").unwrap();
        fs::write(&dir.path().join("broken.md"), &[255]).unwrap();
        let report = audit_memory_repo(dir.path(), &MemoryAuditOptions::default()).unwrap();
        assert_eq!(report.counts["path_orphan"], 2);
        assert_eq!(report.counts["file_unreadable"], 1);
        assert!(report.issues.iter().any(|i| i.path == "bare.md"
            && i.code == "path_orphan"
            && i.detail.contains("no frontmatter")));
        assert!(
            !report
                .issues
                .iter()
                .any(|i| i.path == "broken.md" && i.detail.contains("no frontmatter"))
        );
    }
}
