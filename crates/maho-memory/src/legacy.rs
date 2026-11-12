//! Importer for the legacy Markdown memory store (`~/.omo/memory`, `MAHO_MEMORY_HOME`).
//!
//! Clean-room implementation of `docs/cleanroom/specs/memory-format.md`: walks the store
//! read-only, splits each Markdown file into blocks (headings start sections; top-level list items
//! and paragraphs are blocks), and writes each block as one durable, imported working memory into
//! the given bank. Stable ids make the import idempotent.
use crate::store::{RememberOptions, Store, now_iso, sha256_hex16};
use serde::Serialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Files larger than this are skipped.
pub const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
const IMPORTER: &str = "maho-memory legacy-v1";

/// What to import.
#[derive(Clone, Debug)]
pub struct ImportOptions {
    pub root: PathBuf,
    pub include_people: bool,
    pub dry_run: bool,
    /// Import earlier file revisions from each identity repo's git history. Off by default: it runs
    /// git once per changed file, which is far slower than reading the working tree.
    pub history: bool,
}

/// One problem met during the import.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Warning {
    pub path: String,
    pub reason: String,
}

/// Import summary.
#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct ImportReport {
    pub root: String,
    pub found: bool,
    pub dry_run: bool,
    pub files_scanned: u64,
    pub files_imported: u64,
    pub blocks_imported: u64,
    pub skipped_existing: u64,
    pub skipped_people_files: u64,
    pub skipped_non_markdown: u64,
    /// Files whose readable `expires` instant was not later than the import time.
    pub expired_on_import: u64,
    /// Earlier block revisions recovered from git history, each closed by its successor.
    pub revisions_imported: u64,
    pub warnings: Vec<Warning>,
}

impl ImportReport {
    /// Human-readable summary.
    pub fn text(&self) -> String {
        if !self.found {
            return format!(
                "No legacy memory store at {} — nothing to import.",
                self.root
            );
        }
        let mut out = format!(
            "{} {} memory block(s) from {} of {} Markdown file(s) in {} ({} already present{}{}{}).",
            if self.dry_run {
                "Would import"
            } else {
                "Imported"
            },
            self.blocks_imported,
            self.files_imported,
            self.files_scanned,
            self.root,
            self.skipped_existing,
            if self.skipped_people_files > 0 {
                format!(
                    ", {} people file(s) skipped — enable people memory or pass --include-people",
                    self.skipped_people_files
                )
            } else {
                String::new()
            },
            if self.skipped_non_markdown > 0 {
                format!(
                    ", {} non-Markdown file(s) ignored",
                    self.skipped_non_markdown
                )
            } else {
                String::new()
            },
            format_args!(", {} file(s) expired on import", self.expired_on_import),
        );
        if self.revisions_imported > 0 {
            out.push_str(&format!(
                "\n  {} earlier revision(s) recovered from git history",
                self.revisions_imported
            ));
        }
        for warning in &self.warnings {
            out.push_str(&format!(
                "\n  warning: {}: {}",
                warning.path, warning.reason
            ));
        }
        out
    }
}

/// The legacy store root: `MAHO_MEMORY_HOME`, else `<home>/.omo/memory`.
pub fn default_root(env_home: Option<&str>, home: &Path) -> PathBuf {
    match env_home.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => PathBuf::from(value),
        None => home.join(".omo").join("memory"),
    }
}

/// Directories under the store root that hold memory content.
const CONTENT_HOMES: [&str; 5] = ["system", "notes", "people", "project", "reference"];

/// One content tree: an identity repository, or the store root itself for a flat store.
struct ContentRoot {
    path: PathBuf,
    /// `agents/<label>/repo` when the tree belongs to one identity; empty for a flat store.
    label: String,
}

/// Finds the content trees of a store.
///
/// The current layout is `<root>/agents/<safe-id>/repo/`; the older flat layout is `<root>/`
/// directly. Both are supported so an upgrade in either direction still imports.
fn content_roots(root: &Path) -> Vec<ContentRoot> {
    let agents = root.join("agents");
    if !agents.is_dir() {
        return vec![ContentRoot {
            path: root.to_path_buf(),
            label: String::new(),
        }];
    }
    let mut identities: Vec<PathBuf> = match std::fs::read_dir(&agents) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .collect(),
        Err(_) => Vec::new(),
    };
    identities.sort();
    let mut roots = Vec::new();
    for identity in identities {
        let Some(name) = identity
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
        else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        let repo = identity.join("repo");
        if repo.is_dir() {
            roots.push(ContentRoot {
                path: repo,
                label: name,
            });
        }
    }
    roots
}

/// A parsed Markdown file.
#[derive(Clone, Debug, PartialEq)]
pub struct ParsedFile {
    pub frontmatter: Option<Value>,
    pub blocks: Vec<Block>,
    pub warning: Option<String>,
}

/// One block and the heading it sits under.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub section: Option<String>,
    pub text: String,
}

fn split_frontmatter(text: &str) -> (Option<Value>, &str, Option<String>) {
    let mut frontmatter: Option<Value> = None;
    let mut warning: Option<String> = None;
    let mut rest = text;
    // Real stores contain files with two stacked frontmatter blocks, so each leading block is
    // stripped the same way. Only the first block that parses as a mapping is kept as frontmatter.
    loop {
        let mut lines = rest.split_inclusive('\n');
        let Some(first) = lines.next() else {
            break;
        };
        if first.trim_end_matches(['\r', '\n']) != "---" {
            break;
        }
        let mut offset = first.len();
        let mut closed = None;
        for line in lines {
            if line.trim_end_matches(['\r', '\n']) == "---" {
                closed = Some((offset, line.len()));
                break;
            }
            offset += line.len();
        }
        let Some((close_offset, close_len)) = closed else {
            // No closing delimiter means no reliable point to cut at, so everything left is body.
            warning.get_or_insert_with(|| "frontmatter is not closed; read as body".into());
            break;
        };
        let yaml = &rest[first.len()..close_offset];
        let body = &rest[close_offset + close_len..];
        match serde_yaml::from_str::<serde_yaml::Value>(yaml) {
            Ok(value @ serde_yaml::Value::Mapping(_)) => {
                if frontmatter.is_none() {
                    frontmatter = serde_json::to_value(value).ok();
                }
            }
            Ok(serde_yaml::Value::Null) => {}
            _ => {
                warning.get_or_insert_with(|| {
                    "frontmatter is not a YAML mapping; skipped, body kept".into()
                });
            }
        }
        rest = body;
    }
    (frontmatter, rest, warning)
}

/// Parses an `expires` value into epoch milliseconds (UTC).
///
/// Accepts `YYYY-MM-DD` (midnight UTC) or `YYYY-MM-DDTHH:MM[:SS[.f{1,9}]]` followed by `Z` or
/// `+HH:MM` / `-HH:MM`; surrounding whitespace is ignored and sub-millisecond digits are
/// truncated. Anything else (impossible dates, out-of-range fields, no zone) yields `None`.
pub fn parse_expires(value: &str) -> Option<i64> {
    crate::expiry::parse_expiry_instant(value.trim())
}

/// Formats epoch milliseconds the way the store stamps times (`YYYY-MM-DDTHH:MM:SS.mmmZ`).
pub fn format_instant(millis: i64) -> Option<String> {
    crate::time_format::format_millis(millis)
}

/// The `expires` key of a file's frontmatter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Expiry {
    /// No `expires` key.
    Absent,
    /// A readable value: the instant and the value as written.
    At { millis: i64, written: String },
    /// A value that cannot be read; the file has no expiry.
    Unusable(String),
}

/// Reads the `expires` key of parsed frontmatter.
pub fn expiry_of(frontmatter: Option<&Value>) -> Expiry {
    let Some(value) = frontmatter.and_then(|fm| fm.get("expires")) else {
        return Expiry::Absent;
    };
    match value
        .as_str()
        .and_then(|text| Some((text, parse_expires(text)?)))
    {
        Some((text, millis)) => Expiry::At {
            millis,
            written: text.to_owned(),
        },
        None => Expiry::Unusable(match value {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        }),
    }
}

fn heading_text(line: &str) -> Option<String> {
    let hashes = line.chars().take_while(|ch| *ch == '#').count();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let rest = &line[hashes..];
    if !rest.is_empty() && !rest.starts_with([' ', '\t']) {
        return None;
    }
    Some(rest.trim().trim_end_matches('#').trim().to_owned())
}

fn is_list_item(line: &str) -> bool {
    if line.starts_with("- ") || line.starts_with("* ") || line.starts_with("+ ") {
        return true;
    }
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    digits > 0 && (line[digits..].starts_with(". ") || line[digits..].starts_with(") "))
}

/// Splits a Markdown file per the spec's block rules.
pub fn parse_markdown(text: &str) -> ParsedFile {
    let (frontmatter, body, warning) = split_frontmatter(text);
    let mut blocks = Vec::new();
    let mut section: Option<String> = None;
    let mut current: Vec<&str> = Vec::new();
    let mut in_fence = false;
    let flush = |current: &mut Vec<&str>, blocks: &mut Vec<Block>, section: &Option<String>| {
        let text = current.join("\n").trim().to_owned();
        if !text.is_empty() {
            blocks.push(Block {
                section: section.clone(),
                text,
            });
        }
        current.clear();
    };
    for raw in body.lines() {
        let line = raw.trim_end_matches('\r');
        if line.trim_start().starts_with("```") || line.trim_start().starts_with("~~~") {
            if !in_fence
                && current.first().is_some_and(|first| is_list_item(first))
                && !line.starts_with([' ', '\t'])
            {
                flush(&mut current, &mut blocks, &section);
            }
            in_fence = !in_fence;
            current.push(line);
            continue;
        }
        if in_fence {
            current.push(line);
            continue;
        }
        if let Some(heading) = heading_text(line) {
            flush(&mut current, &mut blocks, &section);
            section = (!heading.is_empty()).then_some(heading);
            continue;
        }
        if line.trim().is_empty() {
            flush(&mut current, &mut blocks, &section);
            continue;
        }
        if is_list_item(line) && !current.is_empty() {
            flush(&mut current, &mut blocks, &section);
        }
        current.push(line);
    }
    flush(&mut current, &mut blocks, &section);
    if blocks.is_empty()
        && let Some(description) = frontmatter
            .as_ref()
            .and_then(|fm| fm.get("description"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
    {
        blocks.push(Block {
            section: None,
            text: description.to_owned(),
        });
    }
    ParsedFile {
        frontmatter,
        blocks,
        warning,
    }
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn month_hint(path: &str) -> Option<String> {
    let name = path.rsplit('/').next()?;
    let bytes = name.as_bytes();
    let shape = bytes.len() >= 7
        && bytes[..4].iter().all(u8::is_ascii_digit)
        && bytes[4] == b'-'
        && bytes[5..7].iter().all(u8::is_ascii_digit);
    if !shape {
        return None;
    }
    let month: u32 = name[5..7].parse().ok()?;
    (1..=12)
        .contains(&month)
        .then(|| format!("{}-01T00:00:00.000Z", &name[..7]))
}

fn modified_iso(path: &Path) -> Option<String> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    let time: chrono::DateTime<chrono::Utc> = modified.into();
    Some(time.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
}

/// Whether a content-root-relative path is memory content rather than bookkeeping.
fn is_content(rel: &str) -> bool {
    if let Some(rest) = rel.strip_prefix("skills/") {
        // Only `<slug>/SKILL.md` is content; helper files beside it are not.
        return rest.len() > "/SKILL.md".len() && rest.ends_with("/SKILL.md");
    }
    CONTENT_HOMES
        .iter()
        .any(|home| rel == *home || rel.starts_with(&format!("{home}/")))
}

struct Kind {
    importance: f64,
    memory_type: &'static str,
    people: bool,
}

fn kind_of(path: &str) -> Kind {
    if path.starts_with("system/") {
        let memory_type = if path == "system/human.md" {
            "identity"
        } else {
            "note"
        };
        Kind {
            importance: 0.9,
            memory_type,
            people: false,
        }
    } else if path.starts_with("notes/facts/") {
        Kind {
            importance: 0.7,
            memory_type: "fact",
            people: false,
        }
    } else if path.starts_with("people/") {
        let importance = if path.ends_with("/card.md") { 0.8 } else { 0.6 };
        Kind {
            importance,
            memory_type: "person",
            people: true,
        }
    } else if path.starts_with("notes/")
        || path.starts_with("project/")
        || path.starts_with("reference/")
    {
        Kind {
            importance: 0.7,
            memory_type: "note",
            people: false,
        }
    } else {
        Kind {
            importance: 0.6,
            memory_type: "note",
            people: false,
        }
    }
}

fn collect_files(root: &Path, dir: &Path, report: &mut ImportReport, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            report.warnings.push(Warning {
                path: relative(root, dir),
                reason: format!("unreadable directory: {error}"),
            });
            return;
        }
    };
    let mut paths: Vec<_> = entries.flatten().map(|entry| entry.path()).collect();
    paths.sort();
    for path in paths {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        if name.starts_with('.') {
            continue;
        }
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            report.warnings.push(Warning {
                path: relative(root, &path),
                reason: "symbolic link not followed".into(),
            });
        } else if meta.is_dir() {
            collect_files(root, &path, report, out);
        } else if path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
        {
            out.push(path);
        } else {
            report.skipped_non_markdown += 1;
        }
    }
}

/// Imports the legacy store at `options.root` into `store`.
pub fn import(store: &Store, options: &ImportOptions) -> Result<ImportReport, String> {
    import_at(store, options, chrono::Utc::now().timestamp_millis())
}

/// [`import`] with an explicit import time (epoch milliseconds, UTC); expiries are judged and
/// missing timestamps filled against it.
pub fn import_at(
    store: &Store,
    options: &ImportOptions,
    now_millis: i64,
) -> Result<ImportReport, String> {
    let root = &options.root;
    let mut report = ImportReport {
        root: root.display().to_string(),
        found: root.is_dir(),
        dry_run: options.dry_run,
        ..Default::default()
    };
    if !report.found {
        return Ok(report);
    }
    let imported_at = format_instant(now_millis).unwrap_or_else(now_iso);
    for content in content_roots(root) {
        let mut files = Vec::new();
        collect_files(&content.path, &content.path, &mut report, &mut files);
        // With history on, the newest revision of a file is its current text, so the working-tree
        // pass only covers files whose repo history could not be read (untracked files, or a root
        // outside any work tree). Importing both would store the same block twice under two
        // different timestamps.
        let covered = if options.history {
            import_history(store, options, &content, &files, &imported_at, &mut report)
        } else {
            Vec::new()
        };
        for path in files {
            if covered.contains(&path) {
                continue;
            }
            import_file(
                store,
                options,
                &content,
                &path,
                now_millis,
                &imported_at,
                &mut report,
            )?;
        }
    }
    Ok(report)
}

/// Recovers earlier revisions of the collected files from each content root's git history.
///
/// The working tree is what a reader sees now; history is what the store can show when asked what
/// changed. Each revision's blocks are inserted with the commit's own timestamp and immediately
/// closed by its successor, so a recall for "now" returns the current text and a recall for a past
/// instant returns the text that was true then. A root that is not a git work tree is skipped, not
/// an error: a flat `--from <dir>` export has no history.
fn import_history(
    store: &Store,
    options: &ImportOptions,
    content: &ContentRoot,
    files: &[PathBuf],
    imported_at: &str,
    report: &mut ImportReport,
) -> Vec<PathBuf> {
    let mut covered: Vec<PathBuf> = Vec::new();
    for path in files {
        let rel = relative(&content.path, path);
        if !content.label.is_empty() && !is_content(&rel) {
            continue;
        }
        if content.label.is_empty()
            && (rel.starts_with("runtime/") || rel.starts_with("transient-runs/"))
        {
            continue;
        }
        let history = match file_history(path) {
            Ok(history) => history,
            Err(reason) => {
                report.warnings.push(Warning { path: rel, reason });
                continue;
            }
        };
        if history.is_empty() {
            continue;
        }
        covered.push(path.clone());
        report.files_imported += 1;
        report.files_scanned += 1;
        // Oldest first, so each revision can be closed by the next one. Revisions that leave a
        // file byte-identical are dropped: a commit touching other files still appears in the
        // file's history, and importing its unchanged text would store the same memory again.
        let mut seen: Vec<&str> = Vec::new();
        let history: Vec<&Revision> = history
            .iter()
            .filter(|revision| {
                if seen.contains(&revision.text.as_str()) {
                    return false;
                }
                seen.push(&revision.text);
                true
            })
            .collect();
        for (index, revision) in history.iter().enumerate() {
            let next = history.get(index + 1);
            let kind = kind_of(&rel);
            let parsed = parse_markdown(&revision.text);
            for (block_index, block) in parsed.blocks.iter().enumerate() {
                let content_text = block.text.clone();
                let id = sha256_hex16(&format!(
                    "{rel}\0{block_index}\0{content_text}\0{}",
                    revision.committed_at_millis
                ));
                if !options.dry_run && store.contains(&id).unwrap_or(false) {
                    report.skipped_existing += 1;
                    continue;
                }
                report.revisions_imported += 1;
                report.blocks_imported += 1;
                if options.dry_run {
                    continue;
                }
                let metadata = json!({
                    "legacy_path": rel,
                    "section": block.section,
                    "block": block_index,
                    "frontmatter": parsed.frontmatter,
                    "importer": IMPORTER,
                    "revision_of": revision.sha,
                    "revision_subject": revision.subject,
                });
                let timestamp = format_instant(revision.committed_at_millis)
                    .unwrap_or_else(|| imported_at.to_owned());
                // A revision is closed only by a successor that is not earlier than it. Git commit
                // times have one-second precision, so two revisions of a fast-moving file routinely
                // share a second; refusing to close on equality would leave both texts active at
                // once, which is worse than a zero-length validity window. What must never happen
                // is a `valid_until` *before* the row's own `timestamp`.
                let closing = next
                    .filter(|n| n.committed_at_millis >= revision.committed_at_millis)
                    .and_then(|n| format_instant(n.committed_at_millis));
                // An `expires` in this revision's own frontmatter ends the row sooner than a
                // successor commit does, and it must win when it is earlier: the file said of
                // itself that it stopped being true then. Taking the minimum keeps both rules
                // without either overwriting the other.
                let expiry = match expiry_of(parsed.frontmatter.as_ref()) {
                    Expiry::At { millis, .. } => format_instant(millis),
                    _ => None,
                };
                let closing = match (closing, expiry) {
                    (Some(a), Some(b)) => Some(if b < a { b } else { a }),
                    (a, b) => a.or(b),
                };
                if let Err(error) = store.remember(
                    &content_text,
                    &RememberOptions {
                        source: "import".into(),
                        importance: kind.importance,
                        timestamp: Some(timestamp),
                        scope: "global".into(),
                        veracity: "imported".into(),
                        trust_tier: Some("IMPORTED".into()),
                        memory_type: kind.memory_type.into(),
                        memory_id: Some(id),
                        valid_until: closing,
                        metadata: Some(metadata),
                        ..Default::default()
                    },
                ) {
                    report.warnings.push(Warning {
                        path: rel.clone(),
                        reason: format!("revision {sha} not stored: {error}", sha = revision.sha),
                    });
                    report.revisions_imported -= 1;
                    report.blocks_imported -= 1;
                }
            }
        }
    }
    covered
}

/// One historical revision of one file.
struct Revision {
    sha: String,
    subject: String,
    committed_at_millis: i64,
    text: String,
}

/// Whether the directory is inside a git work tree, and its top level.
fn git_toplevel(dir: &Path) -> Result<PathBuf, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .arg("rev-parse")
        .arg("--show-toplevel")
        .output()
        .map_err(|error| format!("git is unavailable: {error}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(if stderr.contains("not a git repository") {
            "not a git repository; history skipped".into()
        } else {
            format!("git rev-parse failed: {}", stderr.trim())
        });
    }
    Ok(PathBuf::from(
        String::from_utf8_lossy(&out.stdout).trim().to_owned(),
    ))
}

/// Revisions of one file from whichever git work tree contains it, oldest first.
///
/// The repo is resolved per file rather than per content root: `--from` may point at an identity
/// directory or the store root, neither of which is itself a work tree, while the actual repo is
/// `agents/<id>/repo/` below it. A file outside any work tree has no history and yields nothing.
fn file_history(path: &Path) -> Result<Vec<Revision>, String> {
    let Some(dir) = path.parent() else {
        return Ok(Vec::new());
    };
    let Ok(top) = git_toplevel(dir) else {
        return Ok(Vec::new());
    };
    // `git rev-parse --show-toplevel` reports the work tree's *real* path, so a `/var/...` tempdir
    // comes back as `/private/var/...` on macOS and an 8.3 short name comes back long on Windows.
    // Comparing `path` against that without canonicalizing both sides finds no repository and silently
    // imports zero revisions, so both sides are resolved the same way.
    let top = top.canonicalize().unwrap_or(top);
    let path = &path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let Ok(rel) = path.strip_prefix(&top) else {
        return Ok(Vec::new());
    };
    let rel = rel.to_string_lossy().replace('\\', "/");
    let out = Command::new("git")
        .arg("-C")
        .arg(&top)
        .arg("log")
        .arg("--reverse")
        .arg(crate::git_log::LOG_FORMAT)
        .arg("--")
        .arg(&rel)
        .output()
        .map_err(|error| format!("git log failed: {error}"))?;
    if !out.status.success() {
        return Ok(Vec::new());
    }
    let mut revisions = Vec::new();
    for commit in crate::git_log::parse_log(&String::from_utf8_lossy(&out.stdout)) {
        let Ok(when) = chrono::DateTime::parse_from_rfc3339(&commit.committed_at) else {
            continue;
        };
        let blob = Command::new("git")
            .arg("-C")
            .arg(&top)
            .arg("show")
            .arg(format!("{}:{}", commit.sha, &rel))
            .output()
            .map_err(|error| format!("git show failed: {error}"))?;
        if !blob.status.success() {
            continue;
        }
        let Ok(text) = String::from_utf8(blob.stdout) else {
            continue;
        };
        revisions.push(Revision {
            sha: commit.sha,
            subject: commit.subject,
            committed_at_millis: when.timestamp_millis(),
            text,
        });
    }
    // Commit order is not time order: a rebase or a clock skew can leave a newer commit older in
    // the graph. Closing a revision by its graph successor would then record a `valid_until`
    // earlier than the row's own timestamp, which is impossible in the store's temporal model.
    revisions.sort_by_key(|revision| revision.committed_at_millis);
    Ok(revisions)
}

fn import_file(
    store: &Store,
    options: &ImportOptions,
    content: &ContentRoot,
    path: &Path,
    now_millis: i64,
    imported_at: &str,
    report: &mut ImportReport,
) -> Result<(), String> {
    let rel = relative(&content.path, path);
    // An identity repo has known content homes; a flat `--from <dir>` keeps the historical
    // "every Markdown file" behaviour so arbitrary folders still import.
    let keep = if content.label.is_empty() {
        !rel.starts_with("runtime/") && !rel.starts_with("transient-runs/")
    } else {
        is_content(&rel)
    };
    if !keep {
        return Ok(());
    }
    report.files_scanned += 1;
    let kind = kind_of(&rel);
    if kind.people && !options.include_people {
        report.skipped_people_files += 1;
        return Ok(());
    }
    let rel = if content.label.is_empty() {
        rel
    } else {
        // Keep identities apart: the same path in two stores is two different memories.
        format!("{}/{rel}", content.label)
    };
    if std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0) > MAX_FILE_BYTES {
        report.warnings.push(Warning {
            path: rel,
            reason: "larger than 4 MiB; skipped".into(),
        });
        return Ok(());
    }
    let text = match std::fs::read(path) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(error) => {
            report.warnings.push(Warning {
                path: rel,
                reason: format!("unreadable: {error}"),
            });
            return Ok(());
        }
    };
    let parsed = parse_markdown(&text);
    if let Some(reason) = &parsed.warning {
        report.warnings.push(Warning {
            path: rel.clone(),
            reason: reason.clone(),
        });
    }
    let expiry = expiry_of(parsed.frontmatter.as_ref());
    let (valid_until, expires_written) = match &expiry {
        Expiry::At { millis, written } => (format_instant(*millis), Some(written.clone())),
        Expiry::Unusable(value) => {
            report.warnings.push(Warning {
                path: rel.clone(),
                reason: format!("unusable `expires` value {value:?}; imported without expiry"),
            });
            (None, None)
        }
        Expiry::Absent => (None, None),
    };
    if let Expiry::At { millis, .. } = expiry
        && millis <= now_millis
        && !parsed.blocks.is_empty()
    {
        report.expired_on_import += 1;
    }
    let description = parsed
        .frontmatter
        .as_ref()
        .and_then(|fm| fm.get("description"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned);
    let timestamp = month_hint(&rel)
        .or_else(|| modified_iso(path))
        .unwrap_or_else(|| imported_at.to_owned());
    let mut imported_any = false;
    for (index, block) in parsed.blocks.iter().enumerate() {
        let id = sha256_hex16(&format!("{rel}\0{index}\0{}", block.text));
        if store.contains(&id)? {
            report.skipped_existing += 1;
            continue;
        }
        let content = match (&description, rel.as_str()) {
            (Some(name), _) if rel.ends_with("system/human.md") && block.text != *name => {
                format!("About the user ({name}): {}", block.text)
            }
            _ => block.text.clone(),
        };
        imported_any = true;
        report.blocks_imported += 1;
        if options.dry_run {
            continue;
        }
        let mut metadata = json!({
            "legacy_path": rel,
            "section": block.section,
            "block": index,
            "frontmatter": parsed.frontmatter,
            "importer": IMPORTER,
        });
        if let (Some(written), Some(map)) = (&expires_written, metadata.as_object_mut()) {
            map.insert("expires".into(), Value::String(written.clone()));
        }
        store.remember(
            &content,
            &RememberOptions {
                source: "import".into(),
                importance: kind.importance,
                timestamp: Some(timestamp.clone()),
                scope: "global".into(),
                veracity: "imported".into(),
                trust_tier: Some("IMPORTED".into()),
                memory_type: kind.memory_type.into(),
                memory_id: Some(id),
                valid_until: valid_until.clone(),
                metadata: Some(metadata),
                ..Default::default()
            },
        )?;
    }
    if imported_any {
        report.files_imported += 1;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recall::RecallOptions;

    fn write(root: &Path, rel: &str, text: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, text).expect("write");
    }

    /// `MemoryRow` does not surface `memory_type`; the JSON export does.
    fn memory_type_of(store: &Store, id: &str) -> String {
        store
            .export_json()
            .expect("export")
            .get("working_memory")
            .and_then(Value::as_array)
            .expect("working_memory")
            .iter()
            .find(|row| row.get("id").and_then(Value::as_str) == Some(id))
            .and_then(|row| row.get("memory_type"))
            .and_then(Value::as_str)
            .expect("memory_type")
            .to_owned()
    }

    #[test]
    fn markdown_splits_into_blocks() {
        let parsed = parse_markdown(
            "---\ndescription: Ada\naliases: [ada, A.L.]\n---\n# Facts\n- likes tea\n  every morning\n- uses vim\n\nA paragraph\nspanning lines.\n\n## Code\n```\nx = 1\n\ny = 2\n```\n",
        );
        assert_eq!(
            parsed
                .frontmatter
                .as_ref()
                .and_then(|fm| fm.get("description")),
            Some(&json!("Ada"))
        );
        let texts: Vec<&str> = parsed
            .blocks
            .iter()
            .map(|block| block.text.as_str())
            .collect();
        assert_eq!(
            texts,
            [
                "- likes tea\n  every morning",
                "- uses vim",
                "A paragraph\nspanning lines.",
                "```\nx = 1\n\ny = 2\n```"
            ]
        );
        assert_eq!(parsed.blocks[0].section.as_deref(), Some("Facts"));
        assert_eq!(parsed.blocks[3].section.as_deref(), Some("Code"));
    }

    #[test]
    fn broken_frontmatter_is_body() {
        let parsed = parse_markdown("---\nkey: value\nno end\n");
        assert!(parsed.frontmatter.is_none());
        assert!(parsed.warning.is_some());
        assert_eq!(parsed.blocks.len(), 1);
        let only_description = parse_markdown("---\ndescription: Human\n---\n");
        assert_eq!(only_description.blocks[0].text, "Human");
    }

    #[test]
    fn imports_store_idempotently_and_respects_people_default() {
        let dir = tempfile::tempdir().expect("dir");
        let root = dir.path().join("memory");
        write(
            &root,
            "system/human.md",
            "---\ndescription: Indo\n---\nPrefers Korean replies.\n",
        );
        write(
            &root,
            "notes/facts/2026-10.md",
            "- Deploys happen on Fridays\n- CI uses self-hosted runners\n",
        );
        write(
            &root,
            "people/kim/profile.md",
            "Kim owns the billing service.\n",
        );
        write(&root, ".git/config", "[core]\n");
        write(&root, "notes/image.png", "binary");
        let store = Store::open_in_memory("import").expect("store");
        let options = ImportOptions {
            root: root.clone(),
            history: false,
            include_people: false,
            dry_run: false,
        };
        let report = import(&store, &options).expect("import");
        assert_eq!(report.files_scanned, 3);
        assert_eq!(report.blocks_imported, 3);
        assert_eq!(report.skipped_people_files, 1);
        assert_eq!(report.skipped_non_markdown, 1);
        let again = import(&store, &options).expect("again");
        assert_eq!((again.blocks_imported, again.skipped_existing), (0, 3));

        let results = store
            .recall(
                "deploys fridays",
                5,
                &RecallOptions {
                    ignore_session_scope: true,
                    ..Default::default()
                },
            )
            .expect("recall");
        assert_eq!(
            results[0].timestamp.as_deref(),
            Some("2026-10-01T00:00:00.000Z")
        );
        assert_eq!(results[0].veracity, "imported");
        let human = store
            .recall("korean replies", 5, &RecallOptions::default())
            .expect("recall");
        assert!(human[0].content.starts_with("About the user (Indo): "));

        let with_people = import(
            &store,
            &ImportOptions {
                include_people: true,
                ..options
            },
        )
        .expect("people");
        assert_eq!(with_people.blocks_imported, 1);
    }

    #[test]
    fn dry_run_writes_nothing_and_missing_root_is_fine() {
        let dir = tempfile::tempdir().expect("dir");
        write(dir.path(), "a.md", "one\n\ntwo\n");
        let store = Store::open_in_memory("import").expect("store");
        let report = import(
            &store,
            &ImportOptions {
                root: dir.path().into(),
                history: false,
                include_people: false,
                dry_run: true,
            },
        )
        .expect("dry");
        assert_eq!(report.blocks_imported, 2);
        assert_eq!(store.stats().expect("stats").working, 0);
        let missing = import(
            &store,
            &ImportOptions {
                root: dir.path().join("nope"),
                history: false,
                include_people: false,
                dry_run: false,
            },
        )
        .expect("missing");
        assert!(!missing.found);
        assert!(missing.text().contains("nothing to import"));
    }

    fn instant(iso: &str) -> i64 {
        chrono::DateTime::parse_from_rfc3339(iso)
            .expect("iso")
            .timestamp_millis()
    }

    fn expires_line(line: &str) -> Option<String> {
        let parsed = parse_markdown(&format!("---\n{line}\n---\nbody\n"));
        match expiry_of(parsed.frontmatter.as_ref()) {
            Expiry::At { millis, .. } => format_instant(millis),
            _ => None,
        }
    }

    fn recall_at(store: &Store, query: &str, at: &str) -> Vec<String> {
        store
            .recall(
                query,
                10,
                &RecallOptions {
                    ignore_session_scope: true,
                    valid_at: Some(at.into()),
                    update_recall_counts: false,
                    ..Default::default()
                },
            )
            .expect("recall")
            .into_iter()
            .map(|result| result.id)
            .collect()
    }

    const TRIP: &str = "---\nexpires: 2031-03-14\n---\n- Lisbon flight booked\n- Lisbon hotel near the river\n- Lisbon rental car picked up\n";

    #[test]
    fn expires_date_is_midnight_utc() {
        assert_eq!(
            expires_line("expires: 2031-03-14").as_deref(),
            Some("2031-03-14T00:00:00.000Z")
        );
    }

    #[test]
    fn expires_offset_is_normalised_to_utc() {
        assert_eq!(
            expires_line("expires: 2031-03-14T01:20-05:30").as_deref(),
            Some("2031-03-14T06:50:00.000Z")
        );
    }

    #[test]
    fn expires_quoted_fraction_is_truncated_to_millis() {
        assert_eq!(
            expires_line("expires: '2031-03-14T06:50:09.1234567Z'").as_deref(),
            Some("2031-03-14T06:50:09.123Z")
        );
        assert_eq!(
            expires_line("expires: \"  2031-03-14T06:50:09.9Z \"").as_deref(),
            Some("2031-03-14T06:50:09.900Z")
        );
        assert_eq!(
            expires_line("expires: 2031-03-14T06:50+01:00").as_deref(),
            Some("2031-03-14T05:50:00.000Z")
        );
    }

    #[test]
    fn unreadable_expires_values_mean_no_expiry() {
        for line in [
            "expires: 2031-03-14T06:50:09",
            "expires: 2031-04-31",
            "expires: 2031-03-14T06:61Z",
            "expires: 2031-03-14/06:50Z",
            "expires: when the release ships",
            "expires: [2031-03-14]",
            "expires:",
        ] {
            assert_eq!(expires_line(line), None, "{line}");
        }
        for value in [
            "2031-03-14T24:00Z",
            "2031-03-14T06:50:60Z",
            "2031-03-14T06:50+24:00",
            "2031-03-14T06:50+05:60",
            "2031-03-14T06:50:09.Z",
            "2031-03-14T06:50:09.1234567890Z",
            "2031-03-14 06:50Z",
            "2031-3-14",
            "",
        ] {
            assert_eq!(parse_expires(value), None, "{value}");
        }
        assert_eq!(expiry_of(None), Expiry::Absent);
    }

    #[test]
    fn free_text_expires_imports_blocks_with_one_warning() {
        let dir = tempfile::tempdir().expect("dir");
        write(
            dir.path(),
            "notes/plan.md",
            "---\nexpires: when the release ships\n---\n- first idea\n- second idea\n",
        );
        let store = Store::open_in_memory("import").expect("store");
        let report = import(
            &store,
            &ImportOptions {
                root: dir.path().into(),
                history: false,
                include_people: false,
                dry_run: false,
            },
        )
        .expect("import");
        assert_eq!(report.blocks_imported, 2);
        assert_eq!(report.expired_on_import, 0);
        let warnings: Vec<&Warning> = report
            .warnings
            .iter()
            .filter(|warning| warning.path == "notes/plan.md")
            .collect();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].reason.contains("expires"));
        let first = sha256_hex16("notes/plan.md\u{0}0\u{0}- first idea");
        let row = store.get(&first).expect("get").expect("row");
        assert_eq!(row.valid_until, None);
        let metadata: Value =
            serde_json::from_str(row.metadata.as_deref().expect("metadata")).expect("json");
        assert!(metadata.get("expires").is_none());
        assert_eq!(
            metadata["frontmatter"]["expires"],
            json!("when the release ships")
        );
    }

    #[test]
    fn expiring_file_is_recalled_until_the_exclusive_bound() {
        let dir = tempfile::tempdir().expect("dir");
        write(dir.path(), "notes/trip.md", TRIP);
        let store = Store::open_in_memory("import").expect("store");
        let options = ImportOptions {
            root: dir.path().into(),
            history: false,
            include_people: false,
            dry_run: false,
        };
        let report = import_at(&store, &options, instant("2031-03-10T12:00:00Z")).expect("import");
        assert_eq!(report.blocks_imported, 3);
        assert_eq!(report.expired_on_import, 0);
        let ids: Vec<String> = (0..3)
            .map(|index| {
                let text = parse_markdown(TRIP).blocks[index].text.clone();
                sha256_hex16(&format!("notes/trip.md\0{index}\0{text}"))
            })
            .collect();
        for id in &ids {
            let row = store.get(id).expect("get").expect("row");
            assert_eq!(row.valid_until.as_deref(), Some("2031-03-14T00:00:00.000Z"));
            let metadata: Value =
                serde_json::from_str(row.metadata.as_deref().expect("metadata")).expect("json");
            assert_eq!(metadata["expires"], json!("2031-03-14"));
            assert_eq!(metadata["frontmatter"]["expires"], json!("2031-03-14"));
        }
        let mut before = recall_at(&store, "lisbon", "2031-03-13T23:59:59.999Z");
        before.sort();
        let mut expected = ids.clone();
        expected.sort();
        assert_eq!(before, expected);
        assert!(recall_at(&store, "lisbon", "2031-03-14T00:00:00.000Z").is_empty());

        write(
            dir.path(),
            "notes/trip.md",
            &TRIP.replace("2031-03-14", "2032-01-01"),
        );
        let again = import_at(&store, &options, instant("2031-03-11T00:00:00Z")).expect("again");
        assert_eq!((again.blocks_imported, again.skipped_existing), (0, 3));
        let row = store.get(&ids[0]).expect("get").expect("row");
        assert_eq!(row.valid_until.as_deref(), Some("2031-03-14T00:00:00.000Z"));
    }

    #[test]
    fn file_expired_at_import_is_kept_but_not_recalled() {
        let dir = tempfile::tempdir().expect("dir");
        write(dir.path(), "notes/trip.md", TRIP);
        let store = Store::open_in_memory("import").expect("store");
        let report = import_at(
            &store,
            &ImportOptions {
                root: dir.path().into(),
                history: false,
                include_people: false,
                dry_run: false,
            },
            instant("2031-04-02T00:00:00Z"),
        )
        .expect("import");
        assert_eq!(report.blocks_imported, 3);
        assert_eq!(report.expired_on_import, 1);
        assert_eq!(store.stats().expect("stats").working, 3);
        assert!(recall_at(&store, "lisbon", "2031-04-02T00:00:00.000Z").is_empty());
        let as_json = serde_json::to_value(&report).expect("json");
        assert_eq!(as_json["expired_on_import"], json!(1));
        assert!(report.text().contains("1 file(s) expired on import"));
    }

    #[test]
    fn already_expired_rows_leave_active_counts_and_dry_run_counts_them() {
        let dir = tempfile::tempdir().expect("dir");
        write(
            dir.path(),
            "notes/old.md",
            "---\nexpires: 2020-01-01\n---\nOld sprint goal.\n",
        );
        write(dir.path(), "notes/keep.md", "Permanent note.\n");
        let store = Store::open_in_memory("import").expect("store");
        let options = ImportOptions {
            root: dir.path().into(),
            history: false,
            include_people: false,
            dry_run: true,
        };
        let dry = import(&store, &options).expect("dry");
        assert_eq!(dry.expired_on_import, 1);
        assert_eq!(store.stats().expect("stats").working, 0);
        let report = import(
            &store,
            &ImportOptions {
                dry_run: false,
                ..options
            },
        )
        .expect("import");
        assert_eq!(report.expired_on_import, 1);
        let stats = store.stats().expect("stats");
        assert_eq!((stats.working, stats.working_active), (2, 1));
        let recalled = store
            .recall(
                "sprint goal",
                5,
                &RecallOptions {
                    ignore_session_scope: true,
                    ..Default::default()
                },
            )
            .expect("recall");
        assert!(recalled.is_empty());
    }

    #[test]
    fn tree_without_expires_reports_zero_expired() {
        let dir = tempfile::tempdir().expect("dir");
        write(
            dir.path(),
            "notes/a.md",
            "---\ndescription: x\n---\none\n\ntwo\n",
        );
        let store = Store::open_in_memory("import").expect("store");
        let report = import(
            &store,
            &ImportOptions {
                root: dir.path().into(),
                history: false,
                include_people: false,
                dry_run: false,
            },
        )
        .expect("import");
        assert_eq!(report.expired_on_import, 0);
        assert!(report.warnings.is_empty());
        assert_eq!(
            serde_json::to_value(&report).expect("json")["expired_on_import"],
            json!(0)
        );
        let id = sha256_hex16("notes/a.md\u{0}0\u{0}one");
        assert_eq!(store.get(&id).expect("get").expect("row").valid_until, None);
    }

    #[test]
    fn history_imports_earlier_revisions_closed_by_their_successor() {
        let dir = tempfile::tempdir().expect("dir");
        let root = dir.path();
        let repo = root.join("agents/a/repo");
        write(
            &repo,
            "notes/a.md",
            "---\ndescription: x\n---\nThe first wording.\n",
        );
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .output()
                .expect("git");
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "t@example.com"]);
        run(&["config", "user.name", "T"]);
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "first"]);
        write(
            &repo,
            "notes/a.md",
            "---\ndescription: x\n---\nThe second wording.\n",
        );
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "second"]);

        let store = Store::open_in_memory("import").expect("store");
        let report = import(
            &store,
            &ImportOptions {
                root: root.into(),
                history: true,
                include_people: false,
                dry_run: false,
            },
        )
        .expect("import");
        assert!(
            report.revisions_imported >= 2,
            "both revisions are imported: {}",
            report.revisions_imported
        );
        // The current text is recallable; the replaced wording is retained but closed.
        let hits = store
            .recall("second wording", 5, &RecallOptions::default())
            .expect("recall");
        assert!(
            hits.iter().any(|h| h.content.contains("second wording")),
            "the current revision is recallable"
        );
        let rows = store.export_json().expect("export");
        let all = rows
            .get("working_memory")
            .and_then(Value::as_array)
            .expect("working_memory");
        let first = all
            .iter()
            .find(|r| {
                r.get("content")
                    .and_then(Value::as_str)
                    .is_some_and(|c| c.contains("first wording"))
            })
            .expect("the earlier revision is kept");
        assert!(
            first.get("valid_until").and_then(Value::as_str).is_some(),
            "an earlier revision is closed by its successor: {first}"
        );
    }

    #[test]
    fn history_is_skipped_without_complaint_when_the_root_is_not_a_repo() {
        let dir = tempfile::tempdir().expect("dir");
        write(dir.path(), "a.md", "plain note\n");
        let store = Store::open_in_memory("import").expect("store");
        let report = import(
            &store,
            &ImportOptions {
                root: dir.path().into(),
                history: true,
                include_people: false,
                dry_run: false,
            },
        )
        .expect("import");
        assert_eq!(report.blocks_imported, 1);
        assert!(
            report.warnings.is_empty(),
            "a plain directory is not an error: {:?}",
            report.warnings
        );
        assert_eq!(report.revisions_imported, 0);
    }

    #[test]
    fn recalling_at_a_past_instant_returns_the_text_that_was_true_then() {
        let dir = tempfile::tempdir().expect("dir");
        let root = dir.path();
        let repo = root.join("notes");
        write(&repo, "a.md", "the original wording\n");
        let run = |args: &[&str], date: &str| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .env("GIT_COMMITTER_DATE", date)
                .env("GIT_AUTHOR_DATE", date)
                .output()
                .expect("git");
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        run(&["init", "-q"], "2026-01-01T00:00:00Z");
        run(
            &["config", "user.email", "t@example.com"],
            "2026-01-01T00:00:00Z",
        );
        run(&["config", "user.name", "T"], "2026-01-01T00:00:00Z");
        run(&["add", "."], "2026-01-01T00:00:00Z");
        run(&["commit", "-q", "-m", "first"], "2026-01-01T00:00:00Z");
        write(&repo, "a.md", "the replacement wording\n");
        run(&["add", "."], "2026-06-01T00:00:00Z");
        run(&["commit", "-q", "-m", "second"], "2026-06-01T00:00:00Z");

        let store = Store::open_in_memory("import").expect("store");
        import(
            &store,
            &ImportOptions {
                root: root.into(),
                history: true,
                include_people: false,
                dry_run: false,
            },
        )
        .expect("import");

        // Asking as of March, before the replacement: the old wording is the answer.
        let past = store
            .recall(
                "wording",
                5,
                &RecallOptions {
                    valid_at: Some("2026-03-01T00:00:00Z".into()),
                    query_time: Some("2026-03-01T00:00:00Z".into()),
                    ..Default::default()
                },
            )
            .expect("recall");
        assert!(
            past.iter().any(|hit| hit.content.contains("original")),
            "the past wording is recallable at a past instant: {past:?}"
        );
        assert!(
            !past.iter().any(|hit| hit.content.contains("replacement")),
            "a later wording is not recallable at an earlier instant: {past:?}"
        );

        // Asking as of today: only the current wording.
        let now = store
            .recall("wording", 5, &RecallOptions::default())
            .expect("recall");
        assert!(
            now.iter().any(|hit| hit.content.contains("replacement")),
            "the current wording is recallable now: {now:?}"
        );
        assert!(
            !now.iter().any(|hit| hit.content.contains("original")),
            "a superseded wording is not recallable now: {now:?}"
        );

        // The date-only form is what a person types, and `2026-06-01T00:30:00+09:00` is the same
        // instant as `2026-05-31T15:30:00Z` -- before the June change. Comparing it as text would
        // read the June date part and hand back the replacement instead of the original.
        for given in ["2026-05-31", "2026-06-01T00:30:00+09:00"] {
            let past = store
                .recall(
                    "wording",
                    5,
                    &RecallOptions {
                        valid_at: Some(given.into()),
                        ..Default::default()
                    },
                )
                .expect("recall");
            assert!(
                past.iter().any(|hit| hit.content.contains("original")),
                "{given} selects the wording from that instant: {past:?}"
            );
            assert!(
                !past.iter().any(|hit| hit.content.contains("replacement")),
                "{given} does not see a later wording: {past:?}"
            );
        }
    }

    #[test]
    fn a_valid_at_that_does_not_parse_is_now_rather_than_a_silent_match() {
        let store = Store::open_in_memory("s").expect("store");
        store
            .remember("the release is on tuesday", &RememberOptions::default())
            .expect("remember");
        let hits = store
            .recall(
                "release",
                5,
                &RecallOptions {
                    valid_at: Some("last tuesday".into()),
                    ..Default::default()
                },
            )
            .expect("recall");
        assert_eq!(
            hits.len(),
            1,
            "an unparseable bound falls back to now, so a live row is still visible"
        );
    }

    #[test]
    fn a_revision_expiry_ends_the_row_before_its_successor_does() {
        let dir = tempfile::tempdir().expect("dir");
        let root = dir.path();
        let repo = root.join("notes");
        write(
            &repo,
            "a.md",
            "---\nexpires: 2026-02-01\n---\nThe short-lived wording.\n",
        );
        let run = |args: &[&str], date: &str| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .env("GIT_COMMITTER_DATE", date)
                .env("GIT_AUTHOR_DATE", date)
                .output()
                .expect("git");
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        run(&["init", "-q"], "2026-01-01T00:00:00Z");
        run(
            &["config", "user.email", "t@example.com"],
            "2026-01-01T00:00:00Z",
        );
        run(&["config", "user.name", "T"], "2026-01-01T00:00:00Z");
        run(&["add", "."], "2026-01-01T00:00:00Z");
        run(&["commit", "-q", "-m", "first"], "2026-01-01T00:00:00Z");
        write(
            &repo,
            "a.md",
            "---\nexpires: 2026-02-01\n---\nThe replacement wording.\n",
        );
        run(&["add", "."], "2026-06-01T00:00:00Z");
        run(&["commit", "-q", "-m", "second"], "2026-06-01T00:00:00Z");

        let store = Store::open_in_memory("import").expect("store");
        import(
            &store,
            &ImportOptions {
                root: root.into(),
                history: true,
                include_people: false,
                dry_run: false,
            },
        )
        .expect("import");
        let exported = store.export_json().expect("export");
        let rows = exported
            .get("working_memory")
            .and_then(Value::as_array)
            .expect("rows");
        let first = rows
            .iter()
            .find(|r| {
                r.get("content")
                    .and_then(Value::as_str)
                    .is_some_and(|c| c.contains("short-lived"))
            })
            .expect("the first revision is present");
        let closed = first
            .get("valid_until")
            .and_then(Value::as_str)
            .expect("the first revision is closed");
        assert!(
            closed.starts_with("2026-02-01"),
            "the file's own expiry ends the row, not the later commit: {closed}"
        );
    }

    #[test]
    fn the_importer_and_time_format_module_agree_on_instant_rendering() {
        for millis in [
            0i64,
            1_762_891_200_000,
            1_799_999_999_123,
            -1,
            -86_400_000,
            i64::MAX,
            i64::MIN,
        ] {
            assert_eq!(
                format_instant(millis),
                crate::time_format::format_millis(millis),
                "the two instant formatters disagree on {millis}"
            );
        }
    }

    #[test]
    fn the_importer_and_expiry_module_agree_on_every_spec_case() {
        let cases = [
            "2031-03-14",
            "2031-03-14T00:00",
            "2031-03-14T10:20:30Z",
            "2031-03-14T10:20:30.123Z",
            "2031-03-14T10:20:30.123456789Z",
            "2031-03-14T10:20:30+09:00",
            "2031-03-14T10:20:30-05:30",
            "2031-04-31",
            "2031-03-14T25:00:00Z",
            "2031-03-14T10:20:30",
            "2031-03-14 10:20:30Z",
            "2031-03-14T10:20:60Z",
            "not a date",
            "",
        ];
        for case in cases {
            assert_eq!(
                parse_expires(case),
                crate::expiry::parse_expiry_instant(case),
                "the two expiry parsers disagree on {case:?}"
            );
        }
    }

    #[test]
    fn unparseable_frontmatter_never_becomes_memory_content() {
        let dir = tempfile::tempdir().expect("dir");
        write(
            dir.path(),
            "notes/a.md",
            "---\ndescription: Host (maho-win) deployment: paths and pairing\n---\nThe real note.\n",
        );
        let store = Store::open_in_memory("import").expect("store");
        let report = import(
            &store,
            &ImportOptions {
                root: dir.path().into(),
                history: false,
                include_people: false,
                dry_run: false,
            },
        )
        .expect("import");
        assert_eq!(
            report.warnings.len(),
            1,
            "the unreadable frontmatter is reported"
        );
        let rows = store.recent(10).expect("rows");
        assert_eq!(rows.len(), 1);
        let content = &rows[0].content;
        assert!(
            !content.contains("---"),
            "YAML delimiters must not be stored as memory content: {content}"
        );
        assert!(
            !content.contains("description:"),
            "an unparseable frontmatter must not be promoted to content: {content}"
        );
        assert!(
            content.contains("The real note."),
            "the body survives: {content}"
        );
    }

    #[test]
    fn stacked_frontmatter_blocks_are_all_stripped() {
        let dir = tempfile::tempdir().expect("dir");
        write(
            dir.path(),
            "notes/a.md",
            "---\ndescription: Tart VM live-web E2E: no guest internet\n---\n---\ndescription: Tart VM has no outbound internet (vmnet NAT broken)\n---\n\n# Heading\n\nThe real note.\n",
        );
        let store = Store::open_in_memory("import").expect("store");
        import(
            &store,
            &ImportOptions {
                root: dir.path().into(),
                history: false,
                include_people: false,
                dry_run: false,
            },
        )
        .expect("import");
        let rows = store.recent(10).expect("rows");
        assert_eq!(rows.len(), 1);
        let content = &rows[0].content;
        assert!(
            !content.contains("---"),
            "a second frontmatter block leaked into content: {content}"
        );
        assert!(
            !content.contains("description:"),
            "a second frontmatter block leaked into content: {content}"
        );
        assert!(
            content.contains("The real note."),
            "the body survives: {content}"
        );
    }

    #[test]
    fn agent_layout_imports_each_identity_repo_as_content() {
        let dir = tempfile::tempdir().expect("dir");
        let root = dir.path().join("memory");
        write(
            &root,
            "agents/alpha/repo/system/human.md",
            "---\ndescription: Indo\n---\nPrefers Korean replies.\n",
        );
        write(
            &root,
            "agents/alpha/repo/notes/facts/2026-11.md",
            "- Deploys happen on Fridays\n",
        );
        write(
            &root,
            "agents/alpha/repo/project/plan.md",
            "Ship the importer.\n",
        );
        write(
            &root,
            "agents/alpha/repo/skills/audit/SKILL.md",
            "Audit memory.\n",
        );
        write(
            &root,
            "agents/alpha/repo/skills/audit/notes.txt",
            "helper, not memory\n",
        );
        // Runtime trees beside and inside the store are machinery, not memory.
        write(
            &root,
            "agents/alpha/runtime/reflection/runs/r1/notes.md",
            "reflection scratch\n",
        );
        write(&root, "runtime/transient-runs/x/notes.md", "transient\n");

        let store = Store::open_in_memory("import").expect("store");
        let report = import(
            &store,
            &ImportOptions {
                root: root.clone(),
                history: false,
                include_people: false,
                dry_run: false,
            },
        )
        .expect("import");

        assert_eq!(report.files_scanned, 4, "{report:?}");
        assert_eq!(report.blocks_imported, 4, "{report:?}");
        assert_eq!(report.skipped_non_markdown, 1);
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        assert!(
            report
                .warnings
                .iter()
                .all(|warning| !warning.path.contains("runtime")),
            "runtime must never be scanned: {:?}",
            report.warnings
        );

        // Identity label plus content-relative path, so kind_of sees `system/` again.
        let human_id = sha256_hex16("alpha/system/human.md\u{0}0\u{0}Prefers Korean replies.");
        let human = store.get(&human_id).expect("get").expect("row");
        assert_eq!(memory_type_of(&store, &human_id), "identity");
        assert_eq!(
            human.content,
            "About the user (Indo): Prefers Korean replies."
        );

        let fact_id =
            sha256_hex16("alpha/notes/facts/2026-11.md\u{0}0\u{0}- Deploys happen on Fridays");
        let fact = store.get(&fact_id).expect("get").expect("row");
        assert_eq!(memory_type_of(&store, &fact_id), "fact");
        assert!((fact.importance - 0.7).abs() < f64::EPSILON);
    }

    #[test]
    fn two_identities_with_the_same_path_stay_distinct() {
        let dir = tempfile::tempdir().expect("dir");
        let root = dir.path().join("memory");
        write(&root, "agents/alpha/repo/system/human.md", "Alpha human.\n");
        write(&root, "agents/beta/repo/system/human.md", "Beta human.\n");

        let store = Store::open_in_memory("import").expect("store");
        let report = import(
            &store,
            &ImportOptions {
                root: root.clone(),
                history: false,
                include_people: false,
                dry_run: false,
            },
        )
        .expect("import");

        assert_eq!(report.files_scanned, 2, "{report:?}");
        assert_eq!(report.blocks_imported, 2, "{report:?}");
        assert!(
            store
                .get(&sha256_hex16(
                    "alpha/system/human.md\u{0}0\u{0}Alpha human."
                ))
                .expect("get")
                .is_some()
        );
        assert!(
            store
                .get(&sha256_hex16("beta/system/human.md\u{0}0\u{0}Beta human."))
                .expect("get")
                .is_some()
        );
    }

    #[test]
    fn flat_store_without_agents_still_imports() {
        let dir = tempfile::tempdir().expect("dir");
        write(
            dir.path(),
            "system/human.md",
            "---\ndescription: Ada\n---\nLikes tea.\n",
        );
        write(dir.path(), "project/notes.md", "Project note.\n");

        let store = Store::open_in_memory("import").expect("store");
        let report = import(
            &store,
            &ImportOptions {
                root: dir.path().into(),
                history: false,
                include_people: false,
                dry_run: false,
            },
        )
        .expect("import");

        assert_eq!(report.files_scanned, 2, "{report:?}");
        assert_eq!(report.blocks_imported, 2, "{report:?}");
        let human_id = sha256_hex16("system/human.md\u{0}0\u{0}Likes tea.");
        assert!(store.get(&human_id).expect("get").is_some());
        assert_eq!(memory_type_of(&store, &human_id), "identity");
    }

    #[test]
    fn root_resolution() {
        assert_eq!(
            default_root(Some("/m"), Path::new("/h")),
            PathBuf::from("/m")
        );
        assert_eq!(
            default_root(Some(" "), Path::new("/h")),
            PathBuf::from("/h/.omo/memory")
        );
        assert_eq!(month_hint("notes/facts/2026-13.md"), None);
        assert_eq!(
            month_hint("notes/facts/2026-02-extra.md").as_deref(),
            Some("2026-02-01T00:00:00.000Z")
        );
    }
}
