//! Unified-diff helpers: per-file sections and hunks (port of oh-my-pi `commit/git/diff.ts`
//! `parseFileDiffs`/`parseFileHunks`), the diff-stat rendering (`commit/utils.ts` `renderStat`),
//! review priority and truncation of file diffs (`commit/agentic/tools/git-file-diff.ts`) and the
//! lock-file list (`commit/agentic/lock-files.ts`).
use crate::repo::NumstatEntry;
use serde::Serialize;

/// One `diff --git` section.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileDiff {
    /// Path on the `b/` side.
    pub filename: String,
    /// The whole section, its trailing newline(s) kept.
    pub content: String,
    pub additions: usize,
    pub deletions: usize,
    pub is_binary: bool,
}

/// One `@@` hunk of a file diff.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DiffHunk {
    /// 0-based position in the file's diff.
    pub index: usize,
    pub header: String,
    #[serde(rename = "oldStart")]
    pub old_start: u64,
    #[serde(rename = "oldLines")]
    pub old_lines: u64,
    #[serde(rename = "newStart")]
    pub new_start: u64,
    #[serde(rename = "newLines")]
    pub new_lines: u64,
    pub content: String,
}

/// The hunks of one file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileHunks {
    pub filename: String,
    pub is_binary: bool,
    pub hunks: Vec<DiffHunk>,
}

/// Splits a unified diff at each line starting with `diff --git `, keeping every byte.
pub fn split_sections(diff: &str) -> Vec<&str> {
    let mut starts = Vec::new();
    let mut offset = 0;
    for line in diff.split_inclusive('\n') {
        if line.starts_with("diff --git ") {
            starts.push(offset);
        }
        offset += line.len();
    }
    let mut sections = Vec::new();
    if let Some(first) = starts.first()
        && *first > 0
    {
        sections.push(&diff[..*first]);
    }
    for (i, start) in starts.iter().enumerate() {
        let end = starts.get(i + 1).copied().unwrap_or(diff.len());
        sections.push(&diff[*start..end]);
    }
    if starts.is_empty() && !diff.is_empty() {
        sections.push(diff);
    }
    sections
}

fn header_paths(header: &str) -> Option<(String, String)> {
    let rest = header.strip_prefix("diff --git ")?;
    let rest = rest.trim_end_matches(['\n', '\r']);
    if let Some(stripped) = rest.strip_prefix('"') {
        // Quoted form: "a/x" "b/y".
        let end = stripped.find("\" ")?;
        let a = &stripped[..end];
        let b = stripped[end + 2..].trim_matches('"');
        return Some((
            a.strip_prefix("a/").unwrap_or(a).to_owned(),
            b.strip_prefix("b/").unwrap_or(b).to_owned(),
        ));
    }
    let a_rest = rest.strip_prefix("a/")?;
    // `a/<p> b/<p>`: the separator is the last " b/" (paths may contain spaces).
    let split = a_rest.rfind(" b/")?;
    Some((a_rest[..split].to_owned(), a_rest[split + 3..].to_owned()))
}

/// Paths named on a `diff --git` header line (both sides, deduplicated, `/dev/null` dropped).
pub fn header_files(header: &str) -> Vec<String> {
    let Some((a, b)) = header_paths(header) else {
        return Vec::new();
    };
    let mut files = Vec::new();
    for path in [a, b] {
        if path != "/dev/null" && !path.is_empty() && !files.contains(&path) {
            files.push(path);
        }
    }
    files
}

/// Parses a unified diff into per-file sections.
pub fn parse_file_diffs(diff: &str) -> Vec<FileDiff> {
    let mut files = Vec::new();
    for section in split_sections(diff) {
        if section.trim().is_empty() {
            continue;
        }
        let header = section.lines().next().unwrap_or_default();
        let Some((_, filename)) = header_paths(header) else {
            continue;
        };
        let mut additions = 0;
        let mut deletions = 0;
        let mut is_binary = false;
        for line in section.lines() {
            if line.starts_with("Binary files ") || line == "GIT binary patch" {
                is_binary = true;
            }
            if line.starts_with("+++") || line.starts_with("---") {
                continue;
            }
            if line.starts_with('+') {
                additions += 1;
            } else if line.starts_with('-') {
                deletions += 1;
            }
        }
        files.push(FileDiff {
            filename,
            content: section.to_owned(),
            additions,
            deletions,
            is_binary,
        });
    }
    files
}

fn parse_hunk_header(line: &str) -> (u64, u64, u64, u64) {
    // `@@ -a[,b] +c[,d] @@`
    let inner = line
        .strip_prefix("@@ ")
        .and_then(|rest| rest.split(" @@").next())
        .unwrap_or_default();
    let mut old = (0, 0);
    let mut new = (0, 0);
    for part in inner.split_whitespace() {
        let (sign, range) = part.split_at(1.min(part.len()));
        let mut numbers = range.splitn(2, ',');
        let start = numbers.next().and_then(|n| n.parse().ok()).unwrap_or(0);
        let lines = numbers
            .next()
            .map_or(Some(1), |n| n.parse().ok())
            .unwrap_or(0);
        match sign {
            "-" => old = (start, lines),
            "+" => new = (start, lines),
            _ => {}
        }
    }
    (old.0, old.1, new.0, new.1)
}

/// Splits one file's diff into hunks.
pub fn parse_file_hunks(file: &FileDiff) -> FileHunks {
    if file.is_binary {
        return FileHunks {
            filename: file.filename.clone(),
            is_binary: true,
            hunks: Vec::new(),
        };
    }
    let mut hunks: Vec<DiffHunk> = Vec::new();
    let mut buffer: Vec<&str> = Vec::new();
    for line in file.content.split('\n') {
        if line.starts_with("@@") {
            if let Some(last) = hunks.last_mut() {
                last.content = buffer.join("\n");
            }
            let (old_start, old_lines, new_start, new_lines) = parse_hunk_header(line);
            hunks.push(DiffHunk {
                index: hunks.len(),
                header: line.to_owned(),
                old_start,
                old_lines,
                new_start,
                new_lines,
                content: String::new(),
            });
            buffer = vec![line];
            continue;
        }
        if !hunks.is_empty() {
            buffer.push(line);
        }
    }
    if let Some(last) = hunks.last_mut() {
        last.content = buffer.join("\n");
    }
    FileHunks {
        filename: file.filename.clone(),
        is_binary: false,
        hunks,
    }
}

/// Hunks of every file in `diff`.
pub fn parse_diff_hunks(diff: &str) -> Vec<FileHunks> {
    parse_file_diffs(diff)
        .iter()
        .map(parse_file_hunks)
        .collect()
}

/// `git diff --stat`-like text of numstat rows.
pub fn render_stat(entries: &[NumstatEntry]) -> String {
    if entries.is_empty() {
        return String::new();
    }
    let mut insertions = 0;
    let mut deletions = 0;
    let mut lines = Vec::with_capacity(entries.len() + 1);
    for entry in entries {
        let added = entry.added.unwrap_or(0);
        let removed = entry.removed.unwrap_or(0);
        insertions += added;
        deletions += removed;
        lines.push(format!(
            " {} | {} {}{}",
            entry.path,
            added + removed,
            "+".repeat(usize::try_from(added.min(40)).unwrap_or(40)),
            "-".repeat(usize::try_from(removed.min(40)).unwrap_or(40)),
        ));
    }
    let plural = |n: u64| if n == 1 { "" } else { "s" };
    let files = entries.len();
    lines.push(format!(
        " {files} file{} changed, {insertions} insertion{}(+), {deletions} deletion{}(-)",
        if files == 1 { "" } else { "s" },
        plural(insertions),
        plural(deletions),
    ));
    let mut text = lines.join("\n");
    text.push('\n');
    text
}

/// Machine-written lock files left out of review output.
pub const LOCK_FILES: &[&str] = &[
    "Cargo.lock",
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "bun.lock",
    "bun.lockb",
    "go.sum",
    "poetry.lock",
    "Pipfile.lock",
    "uv.lock",
    "composer.lock",
    "Gemfile.lock",
    "flake.lock",
    "pubspec.lock",
    "Podfile.lock",
    "mix.lock",
    "gradle.lockfile",
];

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

pub fn is_lock_file(path: &str) -> bool {
    LOCK_FILES.contains(&basename(path))
}

fn extension(name: &str) -> &str {
    name.rfind('.').map_or("", |dot| &name[dot..])
}

/// Review priority of a changed file (higher first).
pub fn file_priority(path: &str) -> i32 {
    const SOURCE: &[&str] = &[
        ".rs", ".go", ".py", ".js", ".ts", ".tsx", ".jsx", ".java", ".c", ".cpp", ".h", ".hpp",
    ];
    const SCRIPT: &[&str] = &[".sh", ".bash", ".zsh", ".sql"];
    const MANIFESTS: &[&str] = &[
        "Cargo.toml",
        "package.json",
        "go.mod",
        "pyproject.toml",
        "requirements.txt",
        "Gemfile",
        "build.gradle",
        "pom.xml",
    ];
    const DOCS: &[&str] = &[
        ".md", ".txt", ".json", ".yaml", ".yml", ".toml", ".xml", ".csv",
    ];
    const BINARY: &[&str] = &[
        ".png", ".jpg", ".jpeg", ".gif", ".ico", ".woff", ".woff2", ".ttf", ".eot", ".pdf", ".zip",
        ".tar", ".gz", ".exe", ".dll", ".so", ".dylib",
    ];
    const TESTS: &[&str] = &[
        "/test/",
        "/tests/",
        "/__tests__/",
        "_test.",
        ".test.",
        ".spec.",
        "_spec.",
    ];
    let name = basename(path);
    let ext = extension(name);
    if BINARY.contains(&ext) {
        return -100;
    }
    let lower = path.to_lowercase();
    if TESTS.iter().any(|pattern| lower.contains(pattern)) {
        return 10;
    }
    let manifest = MANIFESTS.contains(&name);
    if DOCS.contains(&ext) && !manifest {
        return 20;
    }
    if manifest {
        return 70;
    }
    if SCRIPT.contains(&ext) {
        return 80;
    }
    if SOURCE.contains(&ext) {
        return 100;
    }
    50
}

/// Diff bodies longer than this many lines are shortened.
pub const TRUNCATE_THRESHOLD_LINES: usize = 30;
const KEEP_HEAD_LINES: usize = 15;
const KEEP_TAIL_LINES: usize = 10;
/// Character budget of one `git_file_diff` answer (about 30k tokens).
pub const MAX_DIFF_CHARS: usize = 120_000;

/// Keeps the first and last lines of a long diff.
pub fn truncate_diff(diff: &str) -> (String, bool) {
    let lines: Vec<&str> = diff.split('\n').collect();
    if lines.len() <= TRUNCATE_THRESHOLD_LINES {
        return (diff.to_owned(), false);
    }
    let elided = lines.len() - KEEP_HEAD_LINES - KEEP_TAIL_LINES;
    let mut out: Vec<String> = lines[..KEEP_HEAD_LINES]
        .iter()
        .map(|l| (*l).to_owned())
        .collect();
    out.push(format!("\n[…{elided} ln elided…]\n"));
    out.extend(
        lines[lines.len() - KEEP_TAIL_LINES..]
            .iter()
            .map(|l| (*l).to_owned()),
    );
    (out.join("\n"), true)
}

/// Orders `files` by priority and renders their diffs within [`MAX_DIFF_CHARS`]; returns the text
/// and the files that were shortened or left out.
pub fn render_file_diffs(
    files: &[String],
    diff_of: impl Fn(&str) -> Option<String>,
) -> (String, Vec<String>) {
    let mut sorted: Vec<&String> = files.iter().collect();
    sorted.sort_by_key(|file| std::cmp::Reverse(file_priority(file)));
    let mut parts = Vec::new();
    let mut truncated = Vec::new();
    let mut total = 0;
    for file in sorted {
        let Some(diff) = diff_of(file).filter(|d| !d.is_empty()) else {
            continue;
        };
        let remaining = MAX_DIFF_CHARS.saturating_sub(total);
        if remaining == 0 {
            truncated.push(file.clone());
            continue;
        }
        let mut content = diff;
        if content.chars().count() > remaining
            || content.split('\n').count() > TRUNCATE_THRESHOLD_LINES
        {
            let (short, was) = truncate_diff(&content);
            if was {
                truncated.push(file.clone());
            }
            content = short;
            let length = content.chars().count();
            if length > remaining {
                let kept: String = content.chars().take(remaining).collect();
                content = format!("{kept}\n[…{} ch elided…]", length - remaining);
                if !truncated.contains(file) {
                    truncated.push(file.clone());
                }
            }
        }
        total += content.chars().count();
        parts.push(format!("=== {file} ===\n{content}"));
    }
    (parts.join("\n\n"), truncated)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIFF: &str = "diff --git a/src/a.rs b/src/a.rs\nindex 1..2 100644\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1,2 +1,2 @@\n-old\n+new\n ctx\n@@ -10 +10,2 @@\n x\n+y\ndiff --git a/img.png b/img.png\nBinary files a/img.png and b/img.png differ\n";

    #[test]
    fn sections_hunks_and_binary() {
        let files = parse_file_diffs(DIFF);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].filename, "src/a.rs");
        assert_eq!((files[0].additions, files[0].deletions), (2, 1));
        assert!(files[1].is_binary);
        assert_eq!(split_sections(DIFF).concat(), DIFF);
        let hunks = parse_file_hunks(&files[0]);
        assert_eq!(hunks.hunks.len(), 2);
        assert_eq!(hunks.hunks[0].header, "@@ -1,2 +1,2 @@");
        assert_eq!(
            (hunks.hunks[1].old_start, hunks.hunks[1].old_lines),
            (10, 1)
        );
        assert_eq!(
            (hunks.hunks[1].new_start, hunks.hunks[1].new_lines),
            (10, 2)
        );
        assert!(
            hunks.hunks[1]
                .content
                .starts_with("@@ -10 +10,2 @@\n x\n+y")
        );
        assert!(parse_file_hunks(&files[1]).hunks.is_empty());
    }

    #[test]
    fn header_paths_with_spaces_and_quotes() {
        assert_eq!(
            header_files("diff --git a/my file.txt b/my file.txt"),
            vec!["my file.txt"]
        );
        assert_eq!(
            header_files("diff --git \"a/x y\" \"b/x z\""),
            vec!["x y", "x z"]
        );
    }

    #[test]
    fn stat_priority_and_truncation() {
        let stat = render_stat(&[
            NumstatEntry {
                path: "a".into(),
                added: Some(2),
                removed: Some(1),
            },
            NumstatEntry {
                path: "b.bin".into(),
                added: None,
                removed: None,
            },
        ]);
        assert_eq!(
            stat,
            " a | 3 ++-\n b.bin | 0 \n 2 files changed, 2 insertions(+), 1 deletion(-)\n"
        );
        assert_eq!(file_priority("src/x.rs"), 100);
        assert_eq!(file_priority("crate/tests/x.rs"), 10);
        assert_eq!(file_priority("Cargo.toml"), 70);
        assert_eq!(file_priority("README.md"), 20);
        assert_eq!(file_priority("logo.png"), -100);
        assert!(is_lock_file("sub/Cargo.lock"));
        let long: String = (0..40).map(|i| format!("l{i}\n")).collect();
        let (short, was) = truncate_diff(&long);
        assert!(
            was && short.contains("[…16 ln elided…]")
                && short.starts_with("l0\n")
                && short.ends_with("l39\n")
        );
        let (text, truncated) = render_file_diffs(&["README.md".into(), "a.rs".into()], |f| {
            Some(format!("d {f}"))
        });
        assert!(text.starts_with("=== a.rs ===\nd a.rs\n\n=== README.md ==="));
        assert!(truncated.is_empty());
    }
}
