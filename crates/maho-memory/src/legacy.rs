//! Importer for the legacy Markdown memory store (`~/.omo/memory`, `MAHO_MEMORY_HOME`).
//!
//! Clean-room implementation of `docs/cleanroom/specs/memory-format.md`: walks the store
//! read-only, splits each Markdown file into blocks (headings start sections; top-level list items
//! and paragraphs are blocks), and writes each block as one durable, imported working memory into
//! the given bank. Stable ids make the import idempotent.
use crate::store::{RememberOptions, Store, now_iso, sha256_hex16};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Files larger than this are skipped.
pub const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
const IMPORTER: &str = "maho-memory legacy-v1";
/// Stands in for a commit sha on the revision read from the working tree.
const WORKTREE_REVISION: &str = "worktree";

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

/// Whether a content-root-relative path is imported at all.
///
/// An identity repo has known content homes; a flat `--from <dir>` keeps the historical "every
/// Markdown file" behaviour so arbitrary folders still import. Both the working-tree and the history
/// pass ask this one function, so neither can import what the other refuses.
fn keeps(content: &ContentRoot, rel: &str) -> bool {
    if content.label.is_empty() {
        !rel.starts_with("runtime/") && !rel.starts_with("transient-runs/")
    } else {
        is_content(rel)
    }
}

/// The `legacy_path` an identity's file is filed under: its label, then the content-relative path.
fn legacy_path_of(content: &ContentRoot, rel: &str) -> String {
    if content.label.is_empty() {
        rel.to_owned()
    } else {
        format!("{}/{rel}", content.label)
    }
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
        // With history on, the history pass also imports a tracked file's uncommitted working copy,
        // so this pass covers only files whose repo history could not be read at all (untracked
        // files, or a root outside any work tree). Importing both would store the same block twice
        // under two different timestamps.
        let covered = if options.history {
            import_history(store, options, &content, &files, &imported_at, &mut report)?
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
) -> Result<Vec<PathBuf>, String> {
    let mut covered: Vec<PathBuf> = Vec::new();
    for path in files {
        let rel = relative(&content.path, path);
        if !keeps(content, &rel) {
            continue;
        }
        let kind = kind_of(&rel);
        // People files are gated by the same policy on both paths: a `people/` file tracked in git
        // is still third-party data, so the working-tree pass reports the skip and this one leaves
        // the file alone rather than importing it behind the caller's back.
        if kind.people && !options.include_people {
            continue;
        }
        // An identity's files are namespaced by its label, exactly as the working-tree pass does:
        // the same path, block and commit second in two identities is two different memories.
        let memory_path = legacy_path_of(content, &rel);
        let history = match file_history(path) {
            Ok(history) => history,
            Err(reason) => {
                report.warnings.push(Warning {
                    path: memory_path,
                    reason,
                });
                continue;
            }
        };
        if history.oversized > 0 {
            report.warnings.push(Warning {
                path: memory_path.clone(),
                reason: format!(
                    "{} git revision(s) larger than 4 MiB; skipped",
                    history.oversized
                ),
            });
        }
        if history.revisions.is_empty() {
            continue;
        }
        covered.push(path.clone());
        report.files_imported += 1;
        report.files_scanned += 1;
        // Oldest first, so each revision can be closed by the next one. A revision is dropped only
        // when it repeats the text immediately before it: comparing against the whole file's text
        // set would also drop a revert (A, B, A leaves B as the current answer).
        let mut revisions: Vec<Revision> = Vec::new();
        for revision in history.revisions {
            if revisions
                .last()
                .is_some_and(|last| last.text == revision.text)
            {
                continue;
            }
            revisions.push(revision);
        }
        let uncommitted = revisions
            .last()
            .and_then(|last| working_tree_revision(path, last, &memory_path, report));
        if let Some(current) = uncommitted {
            revisions.push(current);
        }
        let mut written = BTreeSet::new();
        let mut old_path_reported = false;
        let transaction = if options.dry_run {
            None
        } else {
            Some(
                store
                    .conn
                    .unchecked_transaction()
                    .map_err(crate::store::sql_err)?,
            )
        };
        let mut old_projections = if transaction.is_some() {
            store.worktree_projection_ids(&memory_path, IMPORTER)?
        } else {
            BTreeSet::new()
        };
        for (index, revision) in revisions.iter().enumerate() {
            let Some(text) = &revision.text else { continue };
            let parsed = parse_markdown(text);
            let closing = closing_instant(
                revision,
                revisions.get(index + 1),
                parsed.frontmatter.as_ref(),
            );
            let timestamp = format_instant(revision.committed_at_millis)
                .unwrap_or_else(|| imported_at.to_owned());
            for (block_index, block) in parsed.blocks.iter().enumerate() {
                let content_text = &block.text;
                let base = revision_id(&memory_path, block_index, content_text, revision);
                let id = if revision.sha == WORKTREE_REVISION || !written.insert(base.clone()) {
                    sha256_hex16(&format!("{base}\0{}", revision.sha))
                } else {
                    base
                };
                old_projections.remove(&id);
                if !old_path_reported
                    && memory_path != rel
                    && store.contains(&revision_id(&rel, block_index, content_text, revision))?
                {
                    report.warnings.push(Warning { path: memory_path.clone(), reason: "an earlier import used a key without the identity label; its rows are preserved. Import into a fresh bank to avoid mixing identities".into() });
                    old_path_reported = true;
                }
                if !options.dry_run && store.contains(&id)? {
                    report.skipped_existing += 1;
                    store.close_working_row(&id, closing.as_deref())?;
                } else {
                    report.revisions_imported += 1;
                    report.blocks_imported += 1;
                    if options.dry_run {
                        continue;
                    }
                    let metadata = json!({
                        "legacy_path": memory_path,
                        "section": block.section,
                        "block": block_index,
                        "frontmatter": parsed.frontmatter,
                        "importer": IMPORTER,
                        "revision_of": revision.sha,
                        "revision_subject": revision.subject,
                    });
                    store.remember(
                        content_text,
                        &RememberOptions {
                            source: "import".into(),
                            importance: kind.importance,
                            timestamp: Some(timestamp.clone()),
                            scope: "global".into(),
                            veracity: "imported".into(),
                            trust_tier: Some("IMPORTED".into()),
                            memory_type: kind.memory_type.into(),
                            memory_id: Some(id.clone()),
                            valid_until: closing.clone(),
                            metadata: Some(metadata),
                            ..Default::default()
                        },
                    )?;
                }
                if !options.dry_run
                    && revisions
                        .get(index + 1)
                        .is_some_and(|next| next.sha == WORKTREE_REVISION)
                {
                    store.record_worktree_bound(
                        &id,
                        closing_instant(revision, None, parsed.frontmatter.as_ref()).as_deref(),
                        closing.as_deref(),
                    )?;
                }
            }
        }
        if let Some(transaction) = transaction {
            for stale in old_projections {
                store.forget(&stale)?;
            }
            transaction.commit().map_err(crate::store::sql_err)?;
        }
    }
    Ok(covered)
}

/// The row id of one block of one revision. The importing identity is part of it, so two identities
/// that hold the same text at the same second stay two memories.
fn revision_id(memory_path: &str, block_index: usize, text: &str, revision: &Revision) -> String {
    sha256_hex16(&format!(
        "{memory_path}\0{block_index}\0{text}\0{}",
        revision.committed_at_millis
    ))
}

/// When a revision stops being true: at its successor, or at its own `expires`, whichever is first.
///
/// A revision is closed only by a successor that is not earlier than it. Git commit times have one
/// second of precision, so two revisions of a fast-moving file routinely share a second; refusing to
/// close on equality would leave both texts active at once, which is worse than a zero-length
/// window. What must never happen is a `valid_until` *before* the row's own `timestamp`. An
/// `expires` in this revision's frontmatter ends the row sooner when it is earlier: the file said of
/// itself that it stopped being true then.
fn closing_instant(
    revision: &Revision,
    next: Option<&Revision>,
    frontmatter: Option<&Value>,
) -> Option<String> {
    let successor = next
        .filter(|next| next.committed_at_millis >= revision.committed_at_millis)
        .and_then(|next| format_instant(next.committed_at_millis));
    let expiry = match expiry_of(frontmatter) {
        Expiry::At { millis, .. } => format_instant(millis),
        _ => None,
    };
    match (successor, expiry) {
        (Some(successor), Some(expiry)) => Some(if expiry < successor {
            expiry
        } else {
            successor
        }),
        (successor, expiry) => successor.or(expiry),
    }
}

/// The uncommitted working-tree text as the newest revision, when it differs from the last commit.
///
/// Without this a file with any history is "covered" and its working copy is skipped, so an edit the
/// user has not committed yet — the state they actually see — never reaches the store. Its instant is
/// the file's modification time, which is the best evidence of when the text became current and, being
/// unchanged between runs, keeps a repeated import of a dirty tree idempotent.
fn working_tree_revision(
    path: &Path,
    last: &Revision,
    rel: &str,
    report: &mut ImportReport,
) -> Option<Revision> {
    let meta = std::fs::metadata(path).ok()?;
    let text = if meta.len() > MAX_FILE_BYTES {
        report.warnings.push(Warning {
            path: rel.to_owned(),
            reason: "working tree file larger than 4 MiB; uncommitted content not imported".into(),
        });
        None
    } else {
        match std::fs::read(path) {
            Ok(bytes) => String::from_utf8(bytes).ok(),
            Err(error) => {
                report.warnings.push(Warning {
                    path: rel.to_owned(),
                    reason: format!("unreadable working tree file: {error}"),
                });
                return None;
            }
        }
    };
    if text == last.text {
        return None;
    }
    let modified = meta
        .modified()
        .ok()
        .map(|time| chrono::DateTime::<chrono::Utc>::from(time).timestamp_millis())
        .unwrap_or(last.committed_at_millis);
    // A modification time can predate the last commit (a checkout restores an old one), and a row
    // closed before it begins is not a window the store may record.
    let committed_at_millis = modified.max(last.committed_at_millis);
    Some(Revision {
        sha: WORKTREE_REVISION.to_owned(),
        subject: "uncommitted working tree".into(),
        committed_at_millis,
        text,
    })
}

/// One historical revision of one file.
struct Revision {
    sha: String,
    subject: String,
    committed_at_millis: i64,
    // A skipped blob still ends the previous revision's validity at this commit.
    text: Option<String>,
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

/// What one file's git history gave up, and what it refused.
#[derive(Default)]
struct FileHistory {
    revisions: Vec<Revision>,
    oversized: u64,
}

/// Revisions of one file from whichever git work tree contains it, oldest first.
///
/// The repo is resolved per file rather than per content root: `--from` may point at an identity
/// directory or the store root, neither of which is itself a work tree, while the actual repo is
/// `agents/<id>/repo/` below it. A file outside any work tree has no history and yields nothing.
fn file_history(path: &Path) -> Result<FileHistory, String> {
    let Some(dir) = path.parent() else {
        return Ok(FileHistory::default());
    };
    let Ok(top) = git_toplevel(dir) else {
        return Ok(FileHistory::default());
    };
    // `git rev-parse --show-toplevel` reports the work tree's *real* path, so a `/var/...` tempdir
    // comes back as `/private/var/...` on macOS and an 8.3 short name comes back long on Windows.
    // Comparing `path` against that without canonicalizing both sides finds no repository and silently
    // imports zero revisions, so both sides are resolved the same way.
    let top = top.canonicalize().unwrap_or(top);
    let path = &path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let Ok(rel) = path.strip_prefix(&top) else {
        return Ok(FileHistory::default());
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
        return Ok(FileHistory::default());
    }
    let commits: Vec<_> = crate::git_log::parse_log(&String::from_utf8_lossy(&out.stdout))
        .into_iter()
        .filter(|commit| chrono::DateTime::parse_from_rfc3339(&commit.committed_at).is_ok())
        .collect();
    let specs: Vec<String> = commits
        .iter()
        .map(|commit| format!("{}:{rel}", commit.sha))
        .collect();
    // Size before bytes. A file can read as a few hundred bytes today while one of its revisions is
    // tens of megabytes, and the previous per-revision `git show` had already loaded that revision
    // into memory before anything could reject it.
    //
    // The batch feed is newline-framed, so a path holding a line break would split its own spec and
    // desynchronize every record after it. Such a path falls back to one process per revision, which
    // is slow and correct; every ordinary path keeps the batched read.
    if rel.contains(['\n', '\r']) {
        return Ok(single_blob_history(&top, &rel, &commits));
    }
    let sizes = blob_sizes(&top, &specs)?;
    let wanted: Vec<String> = specs
        .iter()
        .zip(sizes.iter().copied())
        .filter(|(_, size)| within_limit(*size))
        .map(|(spec, _)| spec.clone())
        .collect();
    let blobs = read_blobs(&top, &wanted)?;
    let mut history = FileHistory {
        oversized: sizes
            .iter()
            .flatten()
            .filter(|bytes| **bytes > MAX_FILE_BYTES)
            .count() as u64,
        ..Default::default()
    };
    let mut blobs = blobs.into_iter();
    for (commit, size) in commits.iter().zip(&sizes) {
        let Ok(when) = chrono::DateTime::parse_from_rfc3339(&commit.committed_at) else {
            continue;
        };
        let text = if within_limit(*size) {
            blobs
                .next()
                .flatten()
                .and_then(|bytes| String::from_utf8(bytes).ok())
        } else {
            None
        };
        history.revisions.push(Revision {
            sha: commit.sha.clone(),
            subject: commit.subject.clone(),
            committed_at_millis: when.timestamp_millis(),
            text,
        });
    }
    // Commit order is not time order: a rebase or a clock skew can leave a newer commit older in
    // the graph. Closing a revision by its graph successor would then record a `valid_until`
    // earlier than the row's own timestamp, which is impossible in the store's temporal model.
    history
        .revisions
        .sort_by_key(|revision| revision.committed_at_millis);
    Ok(history)
}

/// Whether git reported a blob size the importer is willing to read.
fn within_limit(size: Option<u64>) -> bool {
    matches!(size, Some(bytes) if bytes <= MAX_FILE_BYTES)
}

/// The slow path for a path the newline-framed batch feed cannot carry: one `git show` per revision.
fn single_blob_history(top: &Path, rel: &str, commits: &[crate::git_log::Commit]) -> FileHistory {
    let mut history = FileHistory::default();
    for commit in commits {
        let Ok(when) = chrono::DateTime::parse_from_rfc3339(&commit.committed_at) else {
            continue;
        };
        let spec = format!("{}:{rel}", commit.sha);
        history.revisions.push(Revision {
            sha: commit.sha.clone(),
            subject: commit.subject.clone(),
            committed_at_millis: when.timestamp_millis(),
            text: None,
        });
        let Ok(size) = Command::new("git")
            .arg("-C")
            .arg(top)
            .arg("cat-file")
            .arg("-s")
            .arg(&spec)
            .output()
        else {
            continue;
        };
        let Ok(size) = String::from_utf8_lossy(&size.stdout).trim().parse::<u64>() else {
            continue;
        };
        if size > MAX_FILE_BYTES {
            history.oversized += 1;
            continue;
        }
        let Ok(blob) = Command::new("git")
            .arg("-C")
            .arg(top)
            .arg("show")
            .arg(&spec)
            .output()
        else {
            continue;
        };
        if !blob.status.success() {
            continue;
        }
        let Ok(text) = String::from_utf8(blob.stdout) else {
            continue;
        };
        if let Some(revision) = history.revisions.last_mut() {
            revision.text = Some(text);
        }
    }
    history
        .revisions
        .sort_by_key(|revision| revision.committed_at_millis);
    history
}

/// Sizes of `<commit>:<path>` specs in order; `None` where git has no such object.
fn blob_sizes(top: &Path, specs: &[String]) -> Result<Vec<Option<u64>>, String> {
    if specs.is_empty() {
        return Ok(Vec::new());
    }
    let output = git_batch(top, "--batch-check", specs)?;
    let text = String::from_utf8_lossy(&output);
    let mut sizes = Vec::with_capacity(specs.len());
    for line in text.lines() {
        let mut fields = line.split(' ');
        let _oid = fields.next();
        let kind = fields.next().unwrap_or_default();
        let size = fields
            .next()
            .and_then(|size| size.parse::<u64>().ok())
            .filter(|_| kind == "blob");
        sizes.push(size);
    }
    sizes.resize(specs.len(), None);
    Ok(sizes)
}

/// Contents of `<commit>:<path>` specs in order; `None` where git has no such object.
fn read_blobs(top: &Path, specs: &[String]) -> Result<Vec<Option<Vec<u8>>>, String> {
    if specs.is_empty() {
        return Ok(Vec::new());
    }
    let output = git_batch(top, "--batch", specs)?;
    let mut blobs = Vec::with_capacity(specs.len());
    let mut offset = 0usize;
    for _ in specs {
        let Some(newline) = output[offset..].iter().position(|byte| *byte == b'\n') else {
            break;
        };
        let header = String::from_utf8_lossy(&output[offset..offset + newline]).into_owned();
        offset += newline + 1;
        let size = header
            .split(' ')
            .nth(2)
            .and_then(|size| size.parse::<usize>().ok());
        let end = size.and_then(|size| offset.checked_add(size));
        let Some(end) = end.filter(|end| *end <= output.len()) else {
            blobs.push(None);
            continue;
        };
        blobs.push(Some(output[offset..end].to_vec()));
        offset = (end + 1).min(output.len());
    }
    blobs.resize(specs.len(), None);
    Ok(blobs)
}

/// Runs one `git cat-file` batch over `specs`, fed on stdin.
fn git_batch(top: &Path, mode: &str, specs: &[String]) -> Result<Vec<u8>, String> {
    let mut input = String::new();
    for spec in specs {
        input.push_str(spec);
        input.push('\n');
    }
    let mut child = Command::new("git")
        .arg("-C")
        .arg(top)
        .arg("cat-file")
        .arg(mode)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("git cat-file failed: {error}"))?;
    // The reply is far larger than a pipe buffer, so writing every spec before reading would block
    // on a full stdout pipe while git blocks on a full stdin pipe. The specs go out from their own
    // thread and the caller drains stdout.
    let mut stdin = child.stdin.take();
    let writer = std::thread::spawn(move || {
        if let Some(pipe) = stdin.as_mut() {
            let _ = pipe.write_all(input.as_bytes());
        }
    });
    let out = child
        .wait_with_output()
        .map_err(|error| format!("git cat-file failed: {error}"))?;
    let _ = writer.join();
    if !out.status.success() {
        return Err(format!(
            "git cat-file {mode} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(out.stdout)
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
    if !keeps(content, &rel) {
        return Ok(());
    }
    report.files_scanned += 1;
    let kind = kind_of(&rel);
    if kind.people && !options.include_people {
        report.skipped_people_files += 1;
        return Ok(());
    }
    let rel = legacy_path_of(content, &rel);
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

    fn git(repo: &Path, args: &[&str], date: &str) {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
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
    }

    fn git_repo(repo: &Path) {
        std::fs::create_dir_all(repo).expect("mkdir");
        git(repo, &["init", "-q"], "2021-01-01T00:00:00Z");
        git(
            repo,
            &["config", "user.email", "t@example.com"],
            "2021-01-01T00:00:00Z",
        );
        git(repo, &["config", "user.name", "T"], "2021-01-01T00:00:00Z");
    }

    fn commit(repo: &Path, message: &str, date: &str) {
        git(repo, &["add", "-A"], date);
        git(repo, &["commit", "-q", "-m", message], date);
    }

    fn history_options(root: &Path, include_people: bool) -> ImportOptions {
        ImportOptions {
            root: root.to_path_buf(),
            history: true,
            include_people,
            dry_run: false,
        }
    }

    fn contents(store: &Store) -> Vec<String> {
        store
            .export_json()
            .expect("export")
            .get("working_memory")
            .and_then(Value::as_array)
            .expect("working_memory")
            .iter()
            .map(|row| {
                row.get("content")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned()
            })
            .collect()
    }

    fn legacy_paths(store: &Store) -> Vec<String> {
        let mut paths: Vec<String> = store
            .export_json()
            .expect("export")
            .get("working_memory")
            .and_then(Value::as_array)
            .expect("working_memory")
            .iter()
            .filter_map(|row| {
                let metadata = row.get("metadata_json")?.as_str()?;
                let metadata: Value = serde_json::from_str(metadata).ok()?;
                Some(metadata.get("legacy_path")?.as_str()?.to_owned())
            })
            .collect();
        paths.sort();
        paths
    }

    #[test]
    fn history_never_imports_a_people_file_without_opt_in() {
        let dir = tempfile::tempdir().expect("dir");
        let repo = dir.path().join("agents/alpha/repo");
        git_repo(&repo);
        write(
            &repo,
            "people/kim/card.md",
            "Kim owns the billing service.\n",
        );
        write(&repo, "notes/facts.md", "- Deploys happen on Fridays\n");
        commit(&repo, "first", "2021-01-01T00:00:00Z");

        let store = Store::open_in_memory("import").expect("store");
        let report = import(&store, &history_options(dir.path(), false)).expect("import");
        assert_eq!(report.skipped_people_files, 1, "{report:?}");
        assert_eq!(contents(&store), ["- Deploys happen on Fridays"]);
        assert!(
            report.warnings.is_empty(),
            "refusing a people file is not a warning: {:?}",
            report.warnings
        );

        let with_people = import(&store, &history_options(dir.path(), true)).expect("people");
        assert!(
            with_people.blocks_imported >= 1,
            "the opt-in imports the tracked people file: {with_people:?}"
        );
        assert!(
            contents(&store)
                .iter()
                .any(|content| content.contains("billing service"))
        );
    }

    #[test]
    fn an_incremental_history_import_closes_the_rows_it_already_wrote() {
        let dir = tempfile::tempdir().expect("dir");
        let repo = dir.path().join("agents/alpha/repo");
        git_repo(&repo);
        write(&repo, "notes/a.md", "The first wording.\n");
        commit(&repo, "first", "2021-01-01T00:00:00Z");

        let store = Store::open_in_memory("import").expect("store");
        let options = history_options(dir.path(), false);
        import(&store, &options).expect("first");
        assert_eq!(store.stats().expect("stats").working_active, 1);

        write(&repo, "notes/a.md", "The second wording.\n");
        commit(&repo, "second", "2021-02-01T00:00:00Z");
        let second = import(&store, &options).expect("second");
        assert!(second.blocks_imported >= 1, "{second:?}");

        let rows = store.recent(10).expect("rows");
        assert_eq!(
            rows.len(),
            1,
            "the successor closes the earlier row instead of leaving both live: {}",
            contents(&store).join(" | ")
        );
        assert!(rows[0].content.contains("second wording"));
        let first = sha256_hex16(&format!(
            "alpha/notes/a.md\u{0}0\u{0}The first wording.\u{0}{}",
            instant("2021-01-01T00:00:00Z")
        ));
        assert_eq!(
            store.get(&first).expect("get").expect("row").valid_until,
            Some("2021-02-01T00:00:00.000Z".into()),
            "the row the first import wrote is closed by the second"
        );
    }

    #[test]
    fn a_reverted_revision_is_the_current_answer_at_the_commit_it_lands() {
        let dir = tempfile::tempdir().expect("dir");
        let repo = dir.path().join("agents/alpha/repo");
        git_repo(&repo);
        write(&repo, "notes/a.md", "The alpha wording.\n");
        commit(&repo, "alpha", "2021-01-01T00:00:00Z");
        write(&repo, "notes/a.md", "The beta wording.\n");
        commit(&repo, "beta", "2021-02-01T00:00:00Z");
        write(&repo, "notes/a.md", "The alpha wording.\n");
        commit(&repo, "alpha again", "2021-03-01T00:00:00Z");

        let store = Store::open_in_memory("import").expect("store");
        import(&store, &history_options(dir.path(), false)).expect("import");

        let now = store.recent(10).expect("rows");
        assert_eq!(
            now.len(),
            1,
            "one answer is live now: {}",
            contents(&store).join(" | ")
        );
        assert!(
            now[0].content.contains("alpha wording"),
            "the reverted text is the current one: {}",
            now[0].content
        );
        let beta = store
            .recall(
                "beta",
                5,
                &RecallOptions {
                    ignore_session_scope: true,
                    ..Default::default()
                },
            )
            .expect("recall");
        assert!(
            beta.is_empty(),
            "the wording that was reverted is no longer current: {beta:?}"
        );
        let before_revert = store
            .recall(
                "beta",
                5,
                &RecallOptions {
                    valid_at: Some("2021-02-15".into()),
                    ignore_session_scope: true,
                    ..Default::default()
                },
            )
            .expect("historical recall");
        assert_eq!(before_revert.len(), 1);
        assert_eq!(before_revert[0].content, "The beta wording.");
    }

    #[test]
    fn a_revert_inside_one_commit_second_leaves_the_returned_text_live() {
        let dir = tempfile::tempdir().expect("dir");
        let repo = dir.path().join("agents/alpha/repo");
        git_repo(&repo);
        let at = "2021-05-05T12:00:00Z";
        let store = Store::open_in_memory("import").expect("store");
        let options = history_options(dir.path(), false);
        write(&repo, "notes/a.md", "The alpha wording.\n");
        commit(&repo, "alpha", at);
        import(&store, &options).expect("initial import");
        write(&repo, "notes/a.md", "The beta wording.\n");
        commit(&repo, "beta", at);
        import(&store, &options).expect("incremental import");
        write(&repo, "notes/a.md", "The alpha wording.\n");
        commit(&repo, "alpha again", at);

        let report = import(&store, &options).expect("import");
        assert_eq!(report.warnings.len(), 0, "{:?}", report.warnings);
        let rows = store.export_json().expect("export");
        let rows = rows
            .get("working_memory")
            .and_then(Value::as_array)
            .expect("working_memory");
        assert_eq!(
            rows.len(),
            3,
            "all three revisions retain their separate records: {rows:?}"
        );
        let live = store.recent(10).expect("rows");
        assert_eq!(live.len(), 1);
        assert!(
            live[0].content.contains("alpha wording"),
            "the text the revert returned to is the live one: {}",
            live[0].content
        );
    }

    #[test]
    fn two_identities_keep_their_history_apart() {
        let dir = tempfile::tempdir().expect("dir");
        let alpha = dir.path().join("agents/alpha/repo");
        let beta = dir.path().join("agents/beta/repo");
        for repo in [&alpha, &beta] {
            git_repo(repo);
            write(repo, "notes/facts.md", "- Deploys happen on Fridays\n");
            commit(repo, "first", "2021-01-01T00:00:00Z");
        }

        let store = Store::open_in_memory("import").expect("store");
        let report = import(&store, &history_options(dir.path(), false)).expect("import");
        assert_eq!(report.blocks_imported, 2, "{report:?}");
        assert_eq!(report.skipped_existing, 0, "{report:?}");
        assert_eq!(
            legacy_paths(&store),
            ["alpha/notes/facts.md", "beta/notes/facts.md"],
            "the same path and commit second in two identities is two memories"
        );
    }

    #[test]
    fn uncommitted_edits_of_a_tracked_file_are_imported_once() {
        let dir = tempfile::tempdir().expect("dir");
        let repo = dir.path().join("agents/alpha/repo");
        git_repo(&repo);
        write(&repo, "notes/a.md", "The committed wording.\n");
        commit(&repo, "first", "2021-01-01T00:00:00Z");
        write(&repo, "notes/a.md", "The uncommitted wording.\n");

        let store = Store::open_in_memory("import").expect("store");
        let options = history_options(dir.path(), false);
        let first = import(&store, &options).expect("first");
        assert!(first.warnings.is_empty(), "{:?}", first.warnings);
        let live = store.recent(10).expect("rows");
        assert_eq!(live.len(), 1);
        assert_eq!(
            live[0].content, "The uncommitted wording.",
            "the text the user actually sees is the current one"
        );
        assert_eq!(store.stats().expect("stats").working, 2);

        let again = import(&store, &options).expect("again");
        assert_eq!(again.blocks_imported, 0, "{again:?}");
        assert_eq!(store.stats().expect("stats").working, 2);
        assert_eq!(store.stats().expect("stats").working_active, 1);
        write(&repo, "notes/a.md", "Another uncommitted wording.\n");
        import(&store, &options).expect("replace projection");
        let current = store.recent(10).expect("current");
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].content, "Another uncommitted wording.");
        write(&repo, "notes/a.md", "The committed wording.\n");
        import(&store, &options).expect("discard projection");
        let current = store.recent(10).expect("current");
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].content, "The committed wording.");
    }

    #[test]
    fn an_oversized_git_revision_is_skipped_before_it_is_read() {
        let dir = tempfile::tempdir().expect("dir");
        let repo = dir.path().join("agents/alpha/repo");
        git_repo(&repo);
        write(&repo, "notes/a.md", "The first wording.\n");
        commit(&repo, "first", "2021-01-01T00:00:00Z");
        write(
            &repo,
            "notes/a.md",
            &"x".repeat(MAX_FILE_BYTES as usize + 1),
        );
        commit(&repo, "huge", "2021-02-01T00:00:00Z");
        let options = history_options(dir.path(), false);
        let incremental = Store::open_in_memory("incremental").expect("store");
        import(&incremental, &options).expect("import with oversized HEAD");
        assert!(incremental.recent(10).expect("current").is_empty());
        write(&repo, "notes/a.md", "The last wording.\n");
        commit(&repo, "last", "2021-03-01T00:00:00Z");

        let store = Store::open_in_memory("import").expect("store");
        let report = import(&store, &history_options(dir.path(), false)).expect("import");
        let revision_warning = report
            .warnings
            .iter()
            .find(|warning| warning.reason.contains("git revision(s) larger"))
            .unwrap_or_else(|| panic!("the oversized revision is reported: {:?}", report.warnings));
        assert_eq!(revision_warning.path, "alpha/notes/a.md");
        assert!(
            revision_warning.reason.contains("1 git revision"),
            "one revision is over the limit, not two: {}",
            revision_warning.reason
        );
        assert_eq!(contents(&store).len(), 2, "{:?}", contents(&store));
        assert!(
            contents(&store).iter().all(|content| content.len() < 1000),
            "no oversized revision reached the store"
        );
        import(&incremental, &options).expect("import successor");
        for db in [&store, &incremental] {
            assert_eq!(recall_at(db, "first", "2021-01-31T23:59:59.999Z").len(), 1);
            assert!(recall_at(db, "first", "2021-02-01T00:00:00Z").is_empty());
            assert!(recall_at(db, "first", "2021-02-15").is_empty());
            assert_eq!(recall_at(db, "last", "2021-03-01").len(), 1);
        }
    }
}
