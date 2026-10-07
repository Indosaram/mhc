//! The visible vocabulary for "memory changed" rows (pin `memory-notice-spec.ts`).

use serde::{Deserialize, Serialize};

use crate::worker::entry_renderers::{NoticeExtraLine, NoticeSpec, join_fields, normalize_renderer_text};

/// Title of a committed write.
pub const REMEMBERED_TITLE: &str = "Remembered";
/// Title of a committed deletion.
pub const LET_GO_TITLE: &str = "Let go";

const STALE_CONSOLIDATION_MS: f64 = 7.0 * 24.0 * 60.0 * 60.0 * 1_000.0;
const UNREFLECTED_WARN_STEPS: u64 = 25;
const DECIMAL_KB_LIMIT: f64 = 10.0 * 1_024.0;

/// One file touched by a committed write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryWriteAffectedFile {
    pub path: String,
    #[serde(default)]
    pub insertions: u64,
    #[serde(default)]
    pub deletions: u64,
}

/// Raw post-commit facts for the visible tool-result row.
///
/// Decoration payload: numbers and identifiers only, no prose and no tone, so the renderer owns every
/// presentation decision. Every optional field is best-effort gathering.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryWriteNotice {
    pub sha: String,
    pub subject: String,
    pub identity: String,
    pub affected: Vec<MemoryWriteAffectedFile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<MemoryWriteNoticeSize>,
    pub timeline: MemoryWriteNoticeTimeline,
}

/// Whole-repository size numbers gathered after a commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryWriteNoticeSize {
    pub system_bytes: u64,
    pub total_bytes: u64,
    pub file_count: u64,
}

/// Timeline numbers gathered after a commit; each is independently optional.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryWriteNoticeTimeline {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entries_today: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_entry_at_iso: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_consolidation_at_iso: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unreflected_steps: Option<u64>,
}

/// Arguments the notice spec reads off the tool call.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemoryNoticeArgs {
    pub command: Option<String>,
    pub file_path: Option<String>,
    pub old_path: Option<String>,
    pub new_path: Option<String>,
}

/// `Remembered · <detail>`; the detail drops out when absent.
pub fn remembered_title(detail: Option<&str>) -> String {
    format!("● {}", join_fields(&[Some(REMEMBERED_TITLE), detail]))
}

/// The committed-write notice.
pub fn memory_write_notice_spec(
    notice: &MemoryWriteNotice,
    now_ms: f64,
    args: &MemoryNoticeArgs,
) -> NoticeSpec {
    let mut extra = Vec::new();
    if let Some(size) = size_line(notice) {
        extra.push(NoticeExtraLine {
            text: size,
            tone: Some("dim".to_string()),
        });
    }
    if let Some(timeline) = timeline_line(notice, now_ms) {
        extra.push(timeline);
    }
    let detail = join_fields(&[
        short_sha(&notice.sha).as_deref(),
        optional(Some(notice.identity.as_str())).as_deref(),
        optional(Some(notice.subject.as_str())).as_deref(),
    ]);
    NoticeSpec {
        glyph: "●".to_string(),
        title: title_line(notice, args),
        tone: "accent".to_string(),
        why: why_line(notice, args),
        extra,
        detail: (!detail.is_empty()).then_some(detail),
    }
}

/// A committed write whose facts could not be gathered: the same notice without the stat lines.
pub fn memory_degraded_notice_spec(args: &MemoryNoticeArgs) -> NoticeSpec {
    memory_write_notice_spec(
        &MemoryWriteNotice {
            timeline: MemoryWriteNoticeTimeline::default(),
            ..Default::default()
        },
        0.0,
        args,
    )
}

/// A refused write: calm and dim, one plain sentence; the raw engine message stays expanded-only.
pub fn memory_failure_notice_spec(message: &str, args: &MemoryNoticeArgs) -> NoticeSpec {
    let raw = normalize_renderer_text(message);
    NoticeSpec {
        glyph: "○".to_string(),
        title: if args.command.as_deref() == Some("delete") {
            "Couldn't let go".to_string()
        } else {
            "Not remembered".to_string()
        },
        tone: "dim".to_string(),
        why: friendly_failure(&raw),
        extra: Vec::new(),
        detail: (!raw.is_empty()).then_some(raw),
    }
}

/// The pending call line while the write runs.
pub fn memory_pending_line(args: &MemoryNoticeArgs) -> String {
    if args.command.as_deref() == Some("delete") {
        return join_fields(&[Some("◌ Letting go"), optional(args.file_path.as_deref())]);
    }
    if args.command.as_deref() == Some("rename") {
        let from = optional(args.old_path.as_deref());
        let to = optional(args.new_path.as_deref());
        let target = match (from.as_deref(), to.as_deref()) {
            (Some(from), Some(to)) => Some(format!("{from} → {to}")),
            (Some(from), None) => Some(from.to_string()),
            (None, to) => to.map(str::to_string),
        };
        return join_fields(&[Some("◌ Moving"), target.as_deref()]);
    }
    join_fields(&[Some("◌ Remembering"), optional(args.file_path.as_deref())])
}

fn title_line(notice: &MemoryWriteNotice, args: &MemoryNoticeArgs) -> String {
    let title = if args.command.as_deref() == Some("delete") {
        LET_GO_TITLE
    } else {
        REMEMBERED_TITLE
    };
    let entries = notice.timeline.entries_today.filter(|entries| *entries > 0);
    match entries {
        Some(entries) => join_fields(&[Some(title), Some(&format!("{} entry today", ordinal(entries)))]),
        None => title.to_string(),
    }
}

/// English ordinals: 1st/2nd/3rd/4th, with the 11-13 exception.
pub fn ordinal(value: u64) -> String {
    let teens = value % 100;
    let suffix = if (11..=13).contains(&teens) {
        "th"
    } else {
        match value % 10 {
            1 => "st",
            2 => "nd",
            3 => "rd",
            _ => "th",
        }
    };
    format!("{value}{suffix}")
}

fn why_line(notice: &MemoryWriteNotice, args: &MemoryNoticeArgs) -> String {
    let affected = &notice.affected;
    let only = if affected.len() == 1 {
        affected.first()
    } else {
        None
    };
    if args.command.as_deref() == Some("delete") {
        let path = optional(args.file_path.as_deref()).or_else(|| {
            only.map(|entry| normalize_renderer_text(&entry.path))
        });
        let cleared = match path {
            None => "a memory".to_string(),
            Some(path) => lines(&path, only.map(|entry| entry.deletions).unwrap_or(0)),
        };
        return format!("Cleared {cleared}. One less thing to carry.");
    }
    if args.command.as_deref() == Some("rename")
        && let (Some(from), Some(to)) = (
            optional(args.old_path.as_deref()),
            optional(args.new_path.as_deref()),
        )
    {
        return format!("Moved {from} to {to}.");
    }
    if affected.is_empty() {
        return match optional(args.file_path.as_deref()) {
            None => "Saved a memory change.".to_string(),
            Some(path) => format!("Saved {path}."),
        };
    }
    if let Some(only) = only
        && only.deletions == 0
        && only.insertions > 0
    {
        let plural = if only.insertions == 1 { "" } else { "s" };
        return format!(
            "Added {} line{plural} to {}.",
            only.insertions,
            normalize_renderer_text(&only.path)
        );
    }
    let paths = affected
        .iter()
        .map(|entry| normalize_renderer_text(&entry.path))
        .collect::<Vec<_>>()
        .join(", ");
    let plural = if affected.len() == 1 { "" } else { "s" };
    format!("Updated {} memory file{plural} ({paths}).", affected.len())
}

fn lines(path: &str, count: u64) -> String {
    if count > 0 {
        let plural = if count == 1 { "" } else { "s" };
        format!("{path} ({count} line{plural})")
    } else {
        path.to_string()
    }
}

fn friendly_failure(raw: &str) -> String {
    let message = strip_command_prefix(raw);
    if let Some(rest) = message
        .strip_prefix("block already exists at ")
        .or_else(|| message.strip_prefix("destination already exists at "))
    {
        return format!("{rest} already exists.");
    }
    if message.starts_with("old_string was not found") {
        return "The text to replace was not in that memory.".to_string();
    }
    if let Some(rest) = message.strip_suffix(" is read_only and cannot be modified") {
        return format!("{rest} is read-only.");
    }
    if message.ends_with("made no changes") || message.ends_with("made no effective changes") {
        return "Nothing needed to change.".to_string();
    }
    if message.starts_with("no memory identity bound") {
        return "Memory is not ready for this session yet.".to_string();
    }
    if let Some(rest) = message
        .strip_prefix('\'')
        .and_then(|rest| rest.strip_suffix("' must be a non-empty string"))
    {
        return format!("The request had no {}.", rest.replace('_', " "));
    }
    if let Some(description) = description_failure(&message) {
        return description;
    }
    if message.is_empty() {
        return "Memory was left unchanged.".to_string();
    }
    let mut chars = message.chars();
    let sentence = match chars.next() {
        Some(first) => format!("{}{}", first.to_uppercase(), chars.as_str()),
        None => message.to_string(),
    };
    if sentence.ends_with(['.', '!', '?']) {
        sentence
    } else {
        format!("{sentence}.")
    }
}

fn strip_command_prefix(raw: &str) -> String {
    let without_memory = raw.strip_prefix("memory: ").unwrap_or(raw);
    match without_memory.split_once(": ") {
        Some((prefix, rest)) if is_command_prefix(prefix) => rest.to_string(),
        _ => without_memory.to_string(),
    }
}

fn is_command_prefix(prefix: &str) -> bool {
    !prefix.is_empty()
        && prefix
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch == '_')
}

fn description_failure(message: &str) -> Option<String> {
    if message.contains("'description' contains tool-call scaffolding") {
        return Some("The memory call arrived garbled, so nothing was saved.".to_string());
    }
    if let Some(rest) = message.split_once("'description' exceeds ") {
        let numbers: Vec<&str> = rest
            .1
            .split(" characters (")
            .collect();
        if let (Some(limit), Some(actual)) = (
            numbers.first(),
            numbers.get(1).and_then(|tail| tail.split(')').next()),
        ) {
            return Some(format!(
                "The description was {actual} characters; the limit is {limit}."
            ));
        }
    }
    if message.contains("'description' must be a single line") {
        return Some("The description has to fit on one line.".to_string());
    }
    if message.contains("'description' must not be empty") {
        return Some("The request had no description.".to_string());
    }
    None
}

fn size_line(notice: &MemoryWriteNotice) -> Option<String> {
    let size = notice.size.as_ref()?;
    let plural = if size.file_count == 1 { "" } else { "s" };
    Some(join_fields(&[
        Some(&format!("system {} injected", format_bytes(size.system_bytes))),
        Some(&format!("{} total", format_bytes(size.total_bytes))),
        Some(&format!("{} file{plural}", size.file_count)),
    ]))
}

fn timeline_line(notice: &MemoryWriteNotice, now_ms: f64) -> Option<NoticeExtraLine> {
    let timeline = &notice.timeline;
    let entry_age = relative_age(timeline.previous_entry_at_iso.as_deref(), now_ms);
    let consolidation_age = relative_age(timeline.last_consolidation_at_iso.as_deref(), now_ms);
    let steps = timeline.unreflected_steps;
    let text = join_fields(&[
        entry_age.as_ref().map(|age| format!("last entry {age}")),
        consolidation_age
            .as_ref()
            .map(|age| format!("last consolidation {age}")),
        steps.map(|steps| {
            let plural = if steps == 1 { "" } else { "s" };
            format!("{steps} step{plural} unreflected")
        }),
    ];
    if text.is_empty() {
        return None;
    }
    let stale = is_stale_consolidation(timeline.last_consolidation_at_iso.as_deref(), now_ms);
    let backlogged = steps.is_some_and(|steps| steps >= UNREFLECTED_WARN_STEPS);
    Some(NoticeExtraLine {
        text,
        tone: Some(if stale || backlogged { "warning" } else { "dim" }.to_string()),
    })
}

fn is_stale_consolidation(iso: Option<&str>, now_ms: f64) -> bool {
    match iso.and_then(parse_iso_millis) {
        Some(at) => now_ms - at >= STALE_CONSOLIDATION_MS,
        None => false,
    }
}

fn relative_age(iso: Option<&str>, now_ms: f64) -> Option<String> {
    let at = parse_iso_millis(iso?)?;
    crate::status::format_relative_age(at, now_ms)
}

fn parse_iso_millis(iso: &str) -> Option<f64> {
    chrono::DateTime::parse_from_rfc3339(iso)
        .ok()
        .map(|at| at.timestamp_millis() as f64)
}

/// One decimal below 10K ("2.0K"), integer above ("33K").
pub fn format_bytes(bytes: u64) -> String {
    let kilobytes = bytes as f64 / 1_024.0;
    if (bytes as f64) < DECIMAL_KB_LIMIT {
        format!("{kilobytes:.1}K")
    } else {
        format!("{}K", kilobytes.round() as u64)
    }
}

/// The first seven normalized characters of a sha, or `None` when empty.
pub fn short_sha(sha: &str) -> Option<String> {
    let normalized = normalize_renderer_text(sha);
    let short: String = normalized.chars().take(7).collect();
    (!short.is_empty()).then_some(short)
}

fn optional(value: Option<&str>) -> Option<String> {
    let value = value?;
    let normalized = normalize_renderer_text(value);
    (!normalized.is_empty()).then_some(normalized)
}

/// Per-path line counts from `git show --numstat -z` (pin `tools.ts` `parseNumstat`).
///
/// `--numstat -z` emits `<ins>\t<del>\t<path>\0` per file, except for renames, which emit
/// `<ins>\t<del>\t\0<old>\0<new>\0`; the destination path is what the notice reports.
pub fn parse_numstat(stdout: &str) -> Vec<MemoryWriteAffectedFile> {
    let fields: Vec<&str> = stdout.split('\0').collect();
    let mut affected = Vec::new();
    let mut index = 0usize;
    while index < fields.len() {
        let field = fields[index];
        if field.trim().is_empty() {
            index += 1;
            continue;
        }
        let parts: Vec<&str> = field.split('\t').collect();
        if parts.len() < 3 {
            index += 1;
            continue;
        }
        let insertions = count_of(parts[0]);
        let deletions = count_of(parts[1]);
        let mut path = parts[2].to_string();
        if path.is_empty() {
            path = fields
                .get(index + 2)
                .or_else(|| fields.get(index + 1))
                .copied()
                .unwrap_or("")
                .to_string();
            index += 2;
        }
        if !path.is_empty() {
            affected.push(MemoryWriteAffectedFile {
                path,
                insertions,
                deletions,
            });
        }
        index += 1;
    }
    affected
}

fn count_of(value: &str) -> u64 {
    value.parse::<u64>().unwrap_or(0)
}

/// Raw post-commit facts for one committed memory write (pin `tools.ts` `gatherMemoryWriteNotice`).
///
/// The affected-file walk is ported; the size and timeline probes read the RPC snapshot builder and
/// are not ported yet, so those two optional fields stay absent and their fragments drop out.
pub fn gather_write_notice(
    repo: &memory_core::git::GitMemoryRepo,
    sha: &str,
    subject: &str,
    identity: &str,
) -> MemoryWriteNotice {
    MemoryWriteNotice {
        sha: sha.to_string(),
        subject: subject.to_string(),
        identity: identity.to_string(),
        affected: read_affected_files(repo, sha),
        size: None,
        timeline: MemoryWriteNoticeTimeline::default(),
    }
}

fn read_affected_files(repo: &memory_core::git::GitMemoryRepo, sha: &str) -> Vec<MemoryWriteAffectedFile> {
    let argv = vec![
        "show".to_string(),
        "--numstat".to_string(),
        "-z".to_string(),
        "--format=".to_string(),
        sha.to_string(),
    ];
    match repo.exec().run_in(&repo.dir, &argv.iter().map(String::as_str).collect::<Vec<_>>()) {
        Ok(result) if result.code == 0 => parse_numstat(&result.stdout),
        _ => Vec::new(),
    }
}

#[cfg(test)]
#[path = "memory_notice_spec_tests.rs"]
mod tests;
