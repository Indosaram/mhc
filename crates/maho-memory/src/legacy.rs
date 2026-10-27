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
            "{} {} memory block(s) from {} of {} Markdown file(s) in {} ({} already present{}{}).",
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
        Kind {
            importance: 0.6,
            memory_type: "person",
            people: true,
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
    let mut files = Vec::new();
    collect_files(root, root, &mut report, &mut files);
    let imported_at = now_iso();
    for path in files {
        let rel = relative(root, &path);
        report.files_scanned += 1;
        let kind = kind_of(&rel);
        if kind.people && !options.include_people {
            report.skipped_people_files += 1;
            continue;
        }
        if std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0) > MAX_FILE_BYTES {
            report.warnings.push(Warning {
                path: rel,
                reason: "larger than 4 MiB; skipped".into(),
            });
            continue;
        }
        let text = match std::fs::read(&path) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(error) => {
                report.warnings.push(Warning {
                    path: rel,
                    reason: format!("unreadable: {error}"),
                });
                continue;
            }
        };
        let parsed = parse_markdown(&text);
        if let Some(reason) = &parsed.warning {
            report.warnings.push(Warning {
                path: rel.clone(),
                reason: reason.clone(),
            });
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
            .or_else(|| modified_iso(&path))
            .unwrap_or_else(|| imported_at.clone());
        let mut imported_any = false;
        for (index, block) in parsed.blocks.iter().enumerate() {
            let id = sha256_hex16(&format!("{rel}\0{index}\0{}", block.text));
            if store.contains(&id)? {
                report.skipped_existing += 1;
                continue;
            }
            let content = match (&description, rel.as_str()) {
                (Some(name), "system/human.md") if block.text != *name => {
                    format!("About the user ({name}): {}", block.text)
                }
                _ => block.text.clone(),
            };
            imported_any = true;
            report.blocks_imported += 1;
            if options.dry_run {
                continue;
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
                    metadata: Some(json!({
                        "legacy_path": rel,
                        "section": block.section,
                        "block": index,
                        "frontmatter": parsed.frontmatter,
                        "importer": IMPORTER,
                    })),
                    ..Default::default()
                },
            )?;
        }
        if imported_any {
            report.files_imported += 1;
        }
    }
    Ok(report)
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
