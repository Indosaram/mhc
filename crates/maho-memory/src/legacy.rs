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

/// Files larger than this are skipped.
pub const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
const IMPORTER: &str = "maho-memory legacy-v1";

/// What to import.
#[derive(Clone, Debug)]
pub struct ImportOptions {
    pub root: PathBuf,
    pub include_people: bool,
    pub dry_run: bool,
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
    let mut lines = text.split_inclusive('\n');
    let Some(first) = lines.next() else {
        return (None, text, None);
    };
    if first.trim_end_matches(['\r', '\n']) != "---" {
        return (None, text, None);
    }
    let mut offset = first.len();
    for line in lines {
        if line.trim_end_matches(['\r', '\n']) == "---" {
            let yaml = &text[first.len()..offset];
            let body = &text[offset + line.len()..];
            return match serde_yaml::from_str::<serde_yaml::Value>(yaml) {
                Ok(value @ serde_yaml::Value::Mapping(_)) => {
                    (serde_json::to_value(value).ok(), body, None)
                }
                Ok(serde_yaml::Value::Null) => (None, body, None),
                _ => (
                    None,
                    text,
                    Some("frontmatter is not a YAML mapping; read as body".into()),
                ),
            };
        }
        offset += line.len();
    }
    (
        None,
        text,
        Some("frontmatter is not closed; read as body".into()),
    )
}

/// Parses an `expires` value into epoch milliseconds (UTC).
///
/// Accepts `YYYY-MM-DD` (midnight UTC) or `YYYY-MM-DDTHH:MM[:SS[.f{1,9}]]` followed by `Z` or
/// `+HH:MM` / `-HH:MM`; surrounding whitespace is ignored and sub-millisecond digits are
/// truncated. Anything else (impossible dates, out-of-range fields, no zone) yields `None`.
pub fn parse_expires(value: &str) -> Option<i64> {
    let text = value.trim();
    let bytes = text.as_bytes();
    let digits = |range: std::ops::Range<usize>| -> Option<u32> {
        let part = bytes.get(range)?;
        if part.is_empty() || !part.iter().all(u8::is_ascii_digit) {
            return None;
        }
        part.iter()
            .try_fold(0u32, |acc, digit| Some(acc * 10 + u32::from(digit - b'0')))
    };
    if bytes.len() < 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let year = i32::try_from(digits(0..4)?).ok()?;
    let date = chrono::NaiveDate::from_ymd_opt(year, digits(5..7)?, digits(8..10)?)?;
    if bytes.len() == 10 {
        return Some(date.and_hms_opt(0, 0, 0)?.and_utc().timestamp_millis());
    }
    if bytes[10] != b'T' || bytes.get(13) != Some(&b':') {
        return None;
    }
    let hour = digits(11..13)?;
    let minute = digits(14..16)?;
    let mut at = 16;
    let mut second = 0;
    let mut millis = 0;
    if bytes.get(at) == Some(&b':') {
        second = digits(at + 1..at + 3)?;
        at += 3;
        if bytes.get(at) == Some(&b'.') {
            let start = at + 1;
            let len = bytes[start..]
                .iter()
                .take_while(|byte| byte.is_ascii_digit())
                .count();
            if !(1..=9).contains(&len) {
                return None;
            }
            let fraction = &text[start..start + len.min(3)];
            millis = format!("{fraction:0<3}").parse().ok()?;
            at = start + len;
        }
    }
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let offset_minutes: i64 = match &bytes[at..] {
        [b'Z'] => 0,
        [sign @ (b'+' | b'-'), _, _, b':', _, _] => {
            let offset_hour = digits(at + 1..at + 3)?;
            let offset_minute = digits(at + 4..at + 6)?;
            if offset_hour > 23 || offset_minute > 59 {
                return None;
            }
            let total = i64::from(offset_hour * 60 + offset_minute);
            if *sign == b'-' { -total } else { total }
        }
        _ => return None,
    };
    let local = date
        .and_hms_milli_opt(hour, minute, second, millis)?
        .and_utc()
        .timestamp_millis();
    Some(local - offset_minutes * 60_000)
}

/// Formats epoch milliseconds the way the store stamps times (`YYYY-MM-DDTHH:MM:SS.mmmZ`).
pub fn format_instant(millis: i64) -> Option<String> {
    chrono::DateTime::from_timestamp_millis(millis)
        .map(|time| time.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
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
        for path in files {
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
