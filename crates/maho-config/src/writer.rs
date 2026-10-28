//! Comment-preserving edits of one configuration file (spec §11).

use crate::cst::{self, Member, Node, NodeKind};
use crate::env::{Env, home_dir};
use chrono::{DateTime, SecondsFormat, Utc};
use maho_support::{parse_jsonc_lenient, write_atomically, write_exclusive};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// One edit: set a value at a key path, or remove a key path.
#[derive(Clone, Debug, PartialEq)]
pub enum Edit {
    Set { path: Vec<String>, value: Value },
    Remove { path: Vec<String> },
}

impl Edit {
    pub fn set(path: &[&str], value: Value) -> Self {
        Edit::Set {
            path: path.iter().map(|s| (*s).to_owned()).collect(),
            value,
        }
    }
    pub fn remove(path: &[&str]) -> Self {
        Edit::Remove {
            path: path.iter().map(|s| (*s).to_owned()).collect(),
        }
    }
}

/// Which file to edit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WriteTarget {
    /// An explicit file path.
    Path(PathBuf),
    /// `<home>/.omo/omo.jsonc`, else an existing `omo.json`.
    User,
    /// `<project>/.omo/omo.jsonc`, else an existing `omo.json`.
    Project(PathBuf),
}

/// Writer options.
#[derive(Clone, Debug)]
pub struct WriteOptions {
    pub target: WriteTarget,
    pub env: Env,
    /// A backup path chosen in advance (reused when it already holds identical content).
    pub planned_backup: Option<PathBuf>,
    /// Clock used for backup names (tests inject it).
    pub now: Option<DateTime<Utc>>,
}

impl WriteOptions {
    pub fn new(target: WriteTarget, env: Env) -> Self {
        Self {
            target,
            env,
            planned_backup: None,
            now: None,
        }
    }
}

/// The failing operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteOp {
    Read,
    Parse,
    Backup,
    Write,
}

/// A writer error. The target file is untouched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriteError {
    pub path: PathBuf,
    pub op: WriteOp,
    pub detail: String,
}

impl std::fmt::Display for WriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?} {}: {}", self.op, self.path.display(), self.detail)
    }
}

impl std::error::Error for WriteError {}

/// What the writer did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriteOutcome {
    pub path: PathBuf,
    pub backup: Option<PathBuf>,
}

/// Header line of a newly created file.
pub const NEW_FILE_HEADER: &str = "// mhc harness configuration";

fn pick_in_dir(dir: &Path) -> PathBuf {
    let jsonc = dir.join("omo.jsonc");
    let json = dir.join("omo.json");
    if !jsonc.exists() && json.exists() {
        json
    } else {
        jsonc
    }
}

/// The ISO-8601 timestamp used in backup names, with `:` and `.` replaced by `-`.
pub fn backup_timestamp(now: DateTime<Utc>) -> String {
    now.to_rfc3339_opts(SecondsFormat::Millis, true)
        .replace([':', '.'], "-")
}

/// `<base>`, else `<base>.1`, `<base>.2`, … — the first path for which `free` holds.
pub fn first_free(base: &Path, free: impl Fn(&Path) -> bool) -> PathBuf {
    if free(base) {
        return base.to_path_buf();
    }
    let mut n = 1;
    loop {
        let candidate = PathBuf::from(format!("{}.{n}", base.display()));
        if free(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

fn err(path: &Path, op: WriteOp, detail: impl Into<String>) -> WriteError {
    WriteError {
        path: path.to_path_buf(),
        op,
        detail: detail.into(),
    }
}

/// Applies `edits` to the target file, keeping comments and formatting.
pub fn update_config(edits: &[Edit], options: &WriteOptions) -> Result<WriteOutcome, WriteError> {
    let path = match &options.target {
        WriteTarget::Path(path) => path.clone(),
        WriteTarget::User => pick_in_dir(&home_dir(&options.env).join(".omo")),
        WriteTarget::Project(project) => {
            let dir = project.join(".omo");
            if maho_support::is_symlink(&dir) {
                return Err(err(
                    &dir,
                    WriteOp::Write,
                    "the project .omo directory is a symlink",
                ));
            }
            pick_in_dir(&dir)
        }
    };
    if maho_support::is_symlink(&path) {
        return Err(err(&path, WriteOp::Write, "the target file is a symlink"));
    }
    let existing = match std::fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(err(&path, WriteOp::Read, error.to_string())),
    };
    let original = match &existing {
        Some(bytes) => String::from_utf8(bytes.clone())
            .map_err(|e| err(&path, WriteOp::Read, e.to_string()))?,
        None => format!("{NEW_FILE_HEADER}\n{{}}\n"),
    };
    let parsed = parse_jsonc_lenient(&original);
    if !parsed.errors.is_empty() {
        let detail: Vec<String> = parsed.errors.iter().map(ToString::to_string).collect();
        return Err(err(&path, WriteOp::Parse, detail.join("; ")));
    }
    let text =
        apply_edits(&original, edits).map_err(|detail| err(&path, WriteOp::Parse, detail))?;
    if existing.is_some() && text == original {
        return Ok(WriteOutcome { path, backup: None });
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| err(&path, WriteOp::Write, e.to_string()))?;
    }
    let backup = match &existing {
        Some(bytes) => Some(write_backup(&path, bytes, options)?),
        None => None,
    };
    write_atomically(&path, text.as_bytes())
        .map_err(|e| err(&path, WriteOp::Write, e.to_string()))?;
    Ok(WriteOutcome { path, backup })
}

fn write_backup(path: &Path, bytes: &[u8], options: &WriteOptions) -> Result<PathBuf, WriteError> {
    let identical =
        |candidate: &Path| std::fs::read(candidate).is_ok_and(|content| content == bytes);
    let base = match &options.planned_backup {
        Some(planned) => {
            if identical(planned) {
                return Ok(planned.clone());
            }
            planned.clone()
        }
        None => {
            let stamp = backup_timestamp(options.now.unwrap_or_else(Utc::now));
            PathBuf::from(format!("{}.bak.{stamp}", path.display()))
        }
    };
    let chosen = first_free(&base, |candidate| {
        !candidate.exists() && !maho_support::is_symlink(candidate)
    });
    write_exclusive(&chosen, bytes).map_err(|e| err(&chosen, WriteOp::Backup, e.to_string()))?;
    Ok(chosen)
}

/// Applies edits to JSONC text (exposed for tests and the migrator).
pub fn apply_edits(original: &str, edits: &[Edit]) -> Result<String, String> {
    let mut text = original.to_owned();
    for edit in edits {
        text = apply_one(&text, edit)?;
    }
    Ok(text)
}

struct Style {
    eol: &'static str,
    unit: String,
}

fn detect_style(text: &str, root: &Node) -> Style {
    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut unit = None;
    if let NodeKind::Object(members) = &root.kind {
        for member in members {
            let indent = line_indent(text, member.key_start);
            if indent.starts_with('\t') {
                unit = Some("\t".to_owned());
                break;
            }
            if !indent.is_empty()
                && unit
                    .as_ref()
                    .is_none_or(|u: &String| indent.len() < u.len())
            {
                unit = Some(indent.to_owned());
            }
        }
    }
    Style {
        eol,
        unit: unit.unwrap_or_else(|| "  ".into()),
    }
}

fn line_start(text: &str, pos: usize) -> usize {
    text[..pos].rfind('\n').map_or(0, |i| i + 1)
}

/// The whitespace that starts the line containing `pos`.
fn line_indent(text: &str, pos: usize) -> &str {
    let start = line_start(text, pos);
    let line = &text[start..pos];
    let len = line.len() - line.trim_start_matches([' ', '\t']).len();
    &line[..len]
}

fn only_whitespace_before(text: &str, pos: usize) -> bool {
    text[line_start(text, pos)..pos]
        .chars()
        .all(|c| c == ' ' || c == '\t')
}

fn serialize(value: &Value, indent: &str, style: &Style) -> String {
    let mut buffer = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(style.unit.as_bytes());
    let mut serializer = serde_json::Serializer::with_formatter(&mut buffer, formatter);
    if serde::Serialize::serialize(value, &mut serializer).is_err() {
        return value.to_string();
    }
    let pretty = String::from_utf8(buffer).unwrap_or_else(|_| value.to_string());
    pretty
        .split('\n')
        .enumerate()
        .map(|(i, line)| {
            if i == 0 {
                line.to_owned()
            } else {
                format!("{indent}{line}")
            }
        })
        .collect::<Vec<_>>()
        .join(style.eol)
}

fn nested(path: &[String], value: &Value) -> Value {
    path.iter().rev().fold(value.clone(), |inner, key| {
        let mut map = serde_json::Map::new();
        map.insert(key.clone(), inner);
        Value::Object(map)
    })
}

fn splice(text: &str, start: usize, end: usize, with: &str) -> String {
    let mut out = String::with_capacity(text.len() + with.len());
    out.push_str(&text[..start]);
    out.push_str(with);
    out.push_str(&text[end..]);
    out
}

fn apply_one(text: &str, edit: &Edit) -> Result<String, String> {
    let root = cst::parse(text).ok_or("the document could not be parsed")?;
    if !matches!(root.kind, NodeKind::Object(_)) {
        return Err("the root is not an object".into());
    }
    let style = detect_style(text, &root);
    match edit {
        Edit::Set { path, value } => {
            if path.is_empty() {
                return Err("empty edit path".into());
            }
            Ok(set_in(text, &root, path, value, &style))
        }
        Edit::Remove { path } => Ok(remove_in(text, &root, path)),
    }
}

fn find<'a>(node: &'a Node, key: &str) -> Option<&'a Member> {
    match &node.kind {
        NodeKind::Object(members) => members.iter().rev().find(|m| m.key == key),
        _ => None,
    }
}

fn set_in(text: &str, object: &Node, path: &[String], value: &Value, style: &Style) -> String {
    let key = &path[0];
    match find(object, key) {
        Some(member) if path.len() == 1 => {
            let indent = line_indent(text, member.key_start).to_owned();
            let rendered = serialize(value, &indent, style);
            splice(text, member.value.start, member.value.end, &rendered)
        }
        Some(member) if matches!(member.value.kind, NodeKind::Object(_)) => {
            set_in(text, &member.value, &path[1..], value, style)
        }
        Some(member) => {
            let indent = line_indent(text, member.key_start).to_owned();
            let rendered = serialize(&nested(&path[1..], value), &indent, style);
            splice(text, member.value.start, member.value.end, &rendered)
        }
        None => insert_member(text, object, key, &nested(&path[1..], value), style),
    }
}

fn insert_member(text: &str, object: &Node, key: &str, value: &Value, style: &Style) -> String {
    let NodeKind::Object(members) = &object.kind else {
        return text.to_owned();
    };
    let key_text = Value::String(key.to_owned()).to_string();
    match members.last() {
        Some(last) => {
            let indent = line_indent(text, last.key_start).to_owned();
            let rendered = format!("{key_text}: {}", serialize(value, &indent, style));
            if let Some(comma) = last.comma {
                // Trailing-comma style: keep it.
                let at = comma + 1;
                let insertion = format!("{}{indent}{rendered},", style.eol);
                let at = end_of_line_if_trivia(text, at).unwrap_or(at);
                splice(text, at, at, &insertion)
            } else {
                let after_value = last.value.end;
                match end_of_line_if_trivia(text, after_value) {
                    Some(eol_at) => {
                        let with_new = splice(
                            text,
                            eol_at,
                            eol_at,
                            &format!("{}{indent}{rendered}", style.eol),
                        );
                        splice(&with_new, after_value, after_value, ",")
                    }
                    None => splice(
                        text,
                        after_value,
                        after_value,
                        &format!(",{}{indent}{rendered}", style.eol),
                    ),
                }
            }
        }
        None => {
            let parent_indent = line_indent(text, object.start).to_owned();
            let indent = format!("{parent_indent}{}", style.unit);
            let rendered = format!("{key_text}: {}", serialize(value, &indent, style));
            let inner = &text[object.start + 1..object.end - 1];
            if inner.trim().is_empty() {
                splice(
                    text,
                    object.start + 1,
                    object.end - 1,
                    &format!(
                        "{}{indent}{rendered}{}{parent_indent}",
                        style.eol, style.eol
                    ),
                )
            } else {
                splice(
                    text,
                    object.start + 1,
                    object.start + 1,
                    &format!("{}{indent}{rendered}", style.eol),
                )
            }
        }
    }
}

/// When the rest of the line after `pos` is only whitespace or a `//` comment, the offset of the
/// line end (before `\r\n` / `\n`).
fn end_of_line_if_trivia(text: &str, pos: usize) -> Option<usize> {
    let rest = &text[pos..];
    let line_end = rest.find('\n').map_or(text.len(), |i| pos + i);
    let line_end = if line_end > pos && text.as_bytes().get(line_end - 1) == Some(&b'\r') {
        line_end - 1
    } else {
        line_end
    };
    let between = text[pos..line_end].trim_start();
    (between.is_empty() || between.starts_with("//")).then_some(line_end)
}

fn remove_in(text: &str, object: &Node, path: &[String]) -> String {
    let Some((key, rest)) = path.split_first() else {
        return text.to_owned();
    };
    let NodeKind::Object(members) = &object.kind else {
        return text.to_owned();
    };
    let Some(index) = members.iter().rposition(|m| &m.key == key) else {
        return text.to_owned();
    };
    let member = &members[index];
    if !rest.is_empty() {
        return remove_in(text, &member.value, rest);
    }
    let start = if only_whitespace_before(text, member.key_start) {
        line_start(text, member.key_start)
    } else {
        member.key_start
    };
    let mut end = member.comma.map_or(member.value.end, |c| c + 1);
    let whole_line = only_whitespace_before(text, member.key_start);
    // A member alone on its line goes with the rest of the line (including a trailing `//`
    // comment, which describes it) and the line break.
    if whole_line && let Some(eol_at) = end_of_line_if_trivia(text, end) {
        end = eol_at;
        if text[end..].starts_with("\r\n") {
            end += 2;
        } else if text[end..].starts_with('\n') {
            end += 1;
        }
    }
    let mut out = splice(text, start, end, "");
    if member.comma.is_none()
        && index > 0
        && let Some(previous_comma) = members[index - 1].comma
    {
        out = splice(&out, previous_comma, previous_comma + 1, "");
    }
    out
}
