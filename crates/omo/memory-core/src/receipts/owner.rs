//! Owner memory receipts and replay suppression (supervisor teach, forget and correct).
//!
//! Both stores are append-only JSONL files in the identity runtime directory: mhc memory storage,
//! outside the memory git repository and never mirrored by a caller.
//!
//! - `owner-receipts.jsonl`: one line per receipt event. A receipt id derives from the owner's
//!   source message id ([`owner_receipt_id`]), so a repeated or replayed request maps to the same
//!   receipt. `accepted` (request taken) precedes `persisted` (names the memory commit), which
//!   precedes `applied` (names a later session whose recalled prompt contained the item; it is not
//!   proof the model obeyed it). `forgotten` and `corrected` close a receipt for good.
//! - `suppressions.jsonl`: normalized lines that facts, reflection, dream and kibitzer output must
//!   never re-introduce, so replaying an old transcript cannot resurrect forgotten or corrected
//!   content. A `lift` line re-enables one record when the owner explicitly teaches it again.
//!   A `purged` line (no text) marks that a forget or correct finished removing content from the
//!   memory repository; the id of the newest one is the suppression generation
//!   ([`suppression_generation`]), and a session whose prompt was pinned under an older generation
//!   repins to the memory HEAD instead of keeping a copy that still carries the removed content.
//!
//! Two matchers, on purpose:
//! - exact ([`restates_exactly`]): the normalized line equals a suppressed line (it may differ only
//!   by case, whitespace, markdown line decoration and trailing punctuation; see
//!   [`normalize_memory_text`]). Used wherever existing memory content is edited or a suppression
//!   is lifted, so forgetting "Use X" never deletes or lifts "Never use X".
//! - containment ([`restates`]): the normalized line equals a suppressed line or contains one of at
//!   least 12 characters. Used only to withhold NEW candidate text (facts records, reflection or
//!   dream lines added by the run, nudges, transcript text handed to the facts child); it may
//!   over-suppress such a candidate, never existing memory.
//!
//! A frontmatter `description` is content too: compile renders it into every prompt, so it is
//! matched like a body line and, when it must go, rewritten to [`NEUTRAL_DESCRIPTION`] (the memory
//! hook rejects an empty one).
//!
//! Both match restatements only. A paraphrase (different words, same meaning) passes every
//! deterministic filter; the only guard against it is the do-not-record list handed to the facts,
//! reflection and dream children ([`render_do_not_record`]), which a model may still disobey.
//!
//! Appends run under the identity's `receipts` lock and are idempotent by event identity.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use super::receipts::under_receipts_lock;
use crate::fs::resilient::{create_dir_all, read_to_string};
use crate::identity::layout::RUNTIME_DIRNAME;
use crate::support::random::random_id;
use crate::support::sha256::sha256_hex;
use crate::support::time::now_iso;

pub const OWNER_RECEIPTS_FILE: &str = "owner-receipts.jsonl";
pub const SUPPRESSIONS_FILE: &str = "suppressions.jsonl";

pub const RECEIPT_ACCEPTED: &str = "accepted";
pub const RECEIPT_PERSISTED: &str = "persisted";
pub const RECEIPT_APPLIED: &str = "applied";
pub const RECEIPT_FORGOTTEN: &str = "forgotten";
pub const RECEIPT_CORRECTED: &str = "corrected";

pub const SUPPRESSION_FORGET: &str = "forget";
pub const SUPPRESSION_CORRECT: &str = "correct";
pub const SUPPRESSION_LIFT: &str = "lift";
pub const SUPPRESSION_PURGED: &str = "purged";

/// Shorter suppressed lines only match exactly, so a short line never swallows unrelated text.
const CONTAINS_MIN_CHARS: usize = 12;
/// Normalized lines shorter than this carry no content worth suppressing.
const SUPPRESS_MIN_CHARS: usize = 4;
/// Trailing sentence punctuation [`normalize_memory_text`] ignores.
const TRAILING_PUNCTUATION: [char; 6] = ['.', '!', ';', ',', '?', ':'];
/// The description a forget or scrub writes in place of one that restates suppressed content.
pub const NEUTRAL_DESCRIPTION: &str = "Memory notes";

/// The receipt id of one owner source message: stable across retries, restarts and replays.
pub fn owner_receipt_id(source_id: &str) -> String {
    let digest = sha256_hex(format!("owner-receipt\u{0}{source_id}").as_bytes());
    format!("rcpt_{}", &digest[..24])
}

/// The trimmed, LF-normalized text an owner taught.
pub fn canonical_text(text: &str) -> String {
    text.replace("\r\n", "\n").trim().to_owned()
}

pub fn text_sha256(text: &str) -> String {
    sha256_hex(canonical_text(text).as_bytes())
}

/// One leading markdown line decoration removed: a blockquote `>`, a heading's `#`s, a list
/// marker (`-`, `*`, `+`, `1.`, `1)`), a checkbox (`[ ]`, `[x]`) or a `[YYYY-MM-DD]` date.
fn strip_line_prefix(text: &str) -> &str {
    let text = text.trim_start();
    if let Some(rest) = text.strip_prefix('>') {
        return rest;
    }
    let hashes = text.len() - text.trim_start_matches('#').len();
    if (1..=6).contains(&hashes) && text[hashes..].starts_with(' ') {
        return &text[hashes..];
    }
    for marker in ["- ", "* ", "+ ", "[ ] ", "[x] ", "[X] "] {
        if let Some(rest) = text.strip_prefix(marker) {
            return rest;
        }
    }
    let digits = text.len() - text.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if (1..=9).contains(&digits) && (text[digits..].starts_with(". ") || text[digits..].starts_with(") ")) {
        return &text[digits + 2..];
    }
    if text.starts_with('[')
        && let Some(end) = text.find(']')
        && text[1..end].chars().all(|c| c.is_ascii_digit() || c == '-')
    {
        return &text[end + 1..];
    }
    text
}

/// The text inside one pair of wrapping emphasis, code or quote marks.
fn strip_wrapping(text: &str) -> &str {
    for (open, close) in [("**", "**"), ("*", "*"), ("`", "`"), ("\"", "\""), ("'", "'"), ("\u{201c}", "\u{201d}")] {
        if text.len() >= open.len() + close.len()
            && let Some(inner) = text.strip_prefix(open).and_then(|rest| rest.strip_suffix(close))
        {
            return inner;
        }
    }
    text
}

/// Lowercased, whitespace-collapsed text without markdown line decoration (blockquote, heading,
/// list marker, checkbox, a leading `[YYYY-MM-DD]` date), wrapping `**`, `*`, backticks or quotes,
/// and trailing `. ! ; , ? :`.
pub fn normalize_memory_text(text: &str) -> String {
    let mut rest = text.trim();
    loop {
        let next = strip_wrapping(strip_line_prefix(rest).trim().trim_end_matches(TRAILING_PUNCTUATION).trim_end()).trim();
        if next == rest {
            break;
        }
        rest = next;
    }
    rest.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// The normalized non-blank body lines (frontmatter excluded) of markdown files: what a tree
/// already holds.
pub fn markdown_body_lines<'a>(files: impl IntoIterator<Item = &'a str>) -> BTreeSet<String> {
    files
        .into_iter()
        .flat_map(|content| split_frontmatter(content).1.split('\n'))
        .map(normalize_memory_text)
        .filter(|line| !line.is_empty())
        .collect()
}

/// [`markdown_body_lines`] plus the normalized frontmatter `description`s: all the content a tree
/// already holds, in one set (a description moved into a body, or back, stays existing content).
pub fn markdown_content_lines<'a>(files: impl IntoIterator<Item = &'a str> + Clone) -> BTreeSet<String> {
    let mut lines = markdown_body_lines(files.clone());
    lines.extend(
        files
            .into_iter()
            .flat_map(|content| description_values(split_frontmatter(content).0))
            .map(normalize_memory_text)
            .filter(|line| !line.is_empty()),
    );
    lines
}

/// The trimmed values of the `description:` lines of a raw frontmatter block.
fn description_values(head: &str) -> Vec<&str> {
    head.split('\n')
        .filter_map(|line| line.split_once(':'))
        .filter(|(key, _)| key.trim() == "description")
        .map(|(_, value)| value.trim())
        .collect()
}

/// Whether `candidate` restates the normalized suppressed line `suppressed`: equal after
/// normalization, or containing it when it is at least 12 characters long. For new candidate text
/// only; editing existing content uses [`restates_exactly`].
pub fn restates(suppressed: &str, candidate: &str) -> bool {
    let candidate = normalize_memory_text(candidate);
    !candidate.is_empty()
        && !suppressed.is_empty()
        && (candidate == suppressed
            || (suppressed.chars().count() >= CONTAINS_MIN_CHARS && candidate.contains(suppressed)))
}

/// Whether `candidate` normalizes to exactly the suppressed line `suppressed` (never a superstring).
pub fn restates_exactly(suppressed: &str, candidate: &str) -> bool {
    !suppressed.is_empty() && normalize_memory_text(candidate) == suppressed
}

/// The distinct normalized lines of `text` worth suppressing.
pub fn suppression_lines(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for line in canonical_text(text).split('\n') {
        let normalized = normalize_memory_text(line);
        if normalized.chars().count() >= SUPPRESS_MIN_CHARS && !lines.contains(&normalized) {
            lines.push(normalized);
        }
    }
    lines
}

/// `(frontmatter block, body)`; the block is empty when the content has no frontmatter. LF and CRLF
/// delimiters are both recognized; the block keeps its bytes verbatim.
pub fn split_frontmatter(content: &str) -> (&str, &str) {
    for (open, close) in [("---\n", "\n---\n"), ("---\r\n", "\r\n---\r\n")] {
        if let Some(rest) = content.strip_prefix(open)
            && let Some(end) = rest.find(close)
        {
            let cut = open.len() + end + close.len();
            return (&content[..cut], &content[cut..]);
        }
    }
    ("", content)
}

/// One line of `owner-receipts.jsonl`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnerReceiptEvent {
    #[serde(default)]
    pub v: u32,
    #[serde(default)]
    pub at: String,
    pub receipt_id: String,
    pub event: String,
    /// `accepted`: the owner source message; closing events: the forget/correct request's source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaced_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl OwnerReceiptEvent {
    pub fn new(receipt_id: &str, event: &str) -> Self {
        Self { v: 1, receipt_id: receipt_id.to_owned(), event: event.to_owned(), ..Default::default() }
    }

    /// At most one line per key: one `applied` per session, one closing event per receipt.
    fn key(&self) -> String {
        match self.event.as_str() {
            RECEIPT_APPLIED => format!("{}\u{0}{RECEIPT_APPLIED}\u{0}{}", self.receipt_id, self.session_id.as_deref().unwrap_or_default()),
            RECEIPT_FORGOTTEN | RECEIPT_CORRECTED => format!("{}\u{0}closed", self.receipt_id),
            event => format!("{}\u{0}{event}", self.receipt_id),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistedMark {
    pub commit: String,
    pub path: Option<String>,
    pub at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppliedMark {
    pub session_id: String,
    /// Memory HEAD when the session's prompt recalled the item.
    pub commit: Option<String>,
    pub at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClosedMark {
    /// `forgotten` | `corrected`.
    pub event: String,
    pub by_source_id: Option<String>,
    /// The commit that removed the content; `None` when nothing was left to remove.
    pub commit: Option<String>,
    pub replaced_by: Option<String>,
    pub reason: Option<String>,
    pub at: String,
}

/// The folded state of one receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnerReceiptView {
    pub receipt_id: String,
    pub source_id: Option<String>,
    /// `accepted` | `persisted` | `applied` | `forgotten` | `corrected`.
    pub state: String,
    pub accepted_at: Option<String>,
    pub accepted_session_id: Option<String>,
    pub text_sha256: Option<String>,
    pub supersedes: Option<String>,
    pub persisted: Option<PersistedMark>,
    pub applied: Vec<AppliedMark>,
    pub closed: Option<ClosedMark>,
    /// The taught text; stays in mhc memory storage and is never serialized to a caller.
    #[serde(skip)]
    pub text: Option<String>,
}

impl OwnerReceiptView {
    fn empty(receipt_id: &str) -> Self {
        Self {
            receipt_id: receipt_id.to_owned(),
            source_id: None,
            state: String::new(),
            accepted_at: None,
            accepted_session_id: None,
            text_sha256: None,
            supersedes: None,
            persisted: None,
            applied: Vec::new(),
            closed: None,
            text: None,
        }
    }

    pub fn is_closed(&self) -> bool {
        self.closed.is_some()
    }
}

/// Folds events (oldest first) into one view per receipt, in first-seen order.
pub fn fold_owner_receipts(events: &[OwnerReceiptEvent]) -> Vec<OwnerReceiptView> {
    let mut order: Vec<String> = Vec::new();
    let mut views: BTreeMap<String, OwnerReceiptView> = BTreeMap::new();
    for event in events {
        let view = views.entry(event.receipt_id.clone()).or_insert_with(|| {
            order.push(event.receipt_id.clone());
            OwnerReceiptView::empty(&event.receipt_id)
        });
        match event.event.as_str() {
            RECEIPT_ACCEPTED if view.accepted_at.is_none() => {
                view.accepted_at = Some(event.at.clone());
                view.source_id = event.source_id.clone();
                view.accepted_session_id = event.session_id.clone();
                view.text = event.text.clone();
                view.text_sha256 = event.text_sha256.clone();
                view.supersedes = event.supersedes.clone();
            }
            RECEIPT_PERSISTED if view.persisted.is_none() => {
                view.persisted = Some(PersistedMark { commit: event.commit.clone().unwrap_or_default(), path: event.path.clone(), at: event.at.clone() });
            }
            RECEIPT_APPLIED => {
                if let Some(session) = &event.session_id
                    && !view.applied.iter().any(|mark| &mark.session_id == session)
                {
                    view.applied.push(AppliedMark { session_id: session.clone(), commit: event.commit.clone(), at: event.at.clone() });
                }
            }
            RECEIPT_FORGOTTEN | RECEIPT_CORRECTED if view.closed.is_none() => {
                view.closed = Some(ClosedMark {
                    event: event.event.clone(),
                    by_source_id: event.source_id.clone(),
                    commit: event.commit.clone(),
                    replaced_by: event.replaced_by.clone(),
                    reason: event.reason.clone(),
                    at: event.at.clone(),
                });
                if view.source_id.is_none() {
                    view.source_id = event.source_id.clone();
                }
                if view.text.is_none() {
                    view.text = event.text.clone();
                }
            }
            _ => {}
        }
    }
    for view in views.values_mut() {
        view.state = match (&view.closed, view.applied.is_empty(), &view.persisted) {
            (Some(closed), _, _) => closed.event.clone(),
            (None, false, _) => RECEIPT_APPLIED.to_owned(),
            (None, true, Some(_)) => RECEIPT_PERSISTED.to_owned(),
            (None, true, None) => RECEIPT_ACCEPTED.to_owned(),
        };
    }
    order.into_iter().filter_map(|id| views.remove(&id)).collect()
}

/// The owner receipt log of one identity.
#[derive(Debug, Clone)]
pub struct OwnerReceiptStore {
    runtime_dir: PathBuf,
}

impl OwnerReceiptStore {
    pub fn new(runtime_dir: &Path) -> Self {
        Self { runtime_dir: runtime_dir.to_path_buf() }
    }

    pub fn path(&self) -> PathBuf {
        self.runtime_dir.join(OWNER_RECEIPTS_FILE)
    }

    /// Every readable event, oldest first; a torn trailing line is skipped.
    pub fn events(&self) -> Result<Vec<OwnerReceiptEvent>, String> {
        read_jsonl(&self.path())
    }

    pub fn views(&self) -> Result<Vec<OwnerReceiptView>, String> {
        Ok(fold_owner_receipts(&self.events()?))
    }

    pub fn view(&self, receipt_id: &str) -> Result<Option<OwnerReceiptView>, String> {
        Ok(self.views()?.into_iter().find(|view| view.receipt_id == receipt_id))
    }

    /// Appends unless an event with the same identity is recorded; the check and the append run
    /// under one lock acquisition. Returns whether a line was written.
    pub fn append_once(&self, mut event: OwnerReceiptEvent) -> Result<bool, String> {
        event.v = 1;
        if event.at.is_empty() {
            event.at = now_iso();
        }
        let key = event.key();
        under_receipts_lock(&self.runtime_dir, || {
            if self.events()?.iter().any(|existing| existing.key() == key) {
                return Ok(false);
            }
            append_line(&self.path(), &event)?;
            Ok(true)
        })
    }
}

/// One line of `suppressions.jsonl`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SuppressionRecord {
    #[serde(default)]
    pub v: u32,
    #[serde(default)]
    pub at: String,
    pub id: String,
    /// `forget` | `correct` | `lift`.
    pub kind: String,
    /// The normalized suppressed line; empty on a `lift` line.
    #[serde(default)]
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    /// The record a `lift` line re-enables.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifts: Option<String>,
    /// On a `purged` line: the digest of the paths that attempt left retained.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retained_digest: Option<String>,
}

impl SuppressionRecord {
    pub fn matches(&self, candidate: &str) -> bool {
        restates(&self.text, candidate)
    }

    pub fn matches_exactly(&self, candidate: &str) -> bool {
        restates_exactly(&self.text, candidate)
    }
}

fn suppressions_path(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join(SUPPRESSIONS_FILE)
}

fn short_digest(prefix: &str, parts: &[&str]) -> String {
    format!("{prefix}{}", &sha256_hex(parts.join("\u{0}").as_bytes())[..24])
}

/// Records one suppression per line of `text` (idempotent per request source and line). Returns
/// the active records for this request.
pub fn append_suppressions(runtime_dir: &Path, kind: &str, text: &str, receipt_id: Option<&str>, source_id: &str) -> Result<Vec<SuppressionRecord>, String> {
    let lines = suppression_lines(text);
    if lines.is_empty() {
        return Err("nothing to suppress: the text has no content line".into());
    }
    under_receipts_lock(runtime_dir, || {
        let existing: BTreeSet<String> = read_jsonl::<SuppressionRecord>(&suppressions_path(runtime_dir))?.into_iter().map(|record| record.id).collect();
        let mut written = Vec::new();
        for line in &lines {
            let record = SuppressionRecord {
                v: 1,
                at: now_iso(),
                id: short_digest("sup_", &[kind, source_id, line.as_str()]),
                kind: kind.to_owned(),
                text: line.clone(),
                receipt_id: receipt_id.map(str::to_owned),
                source_id: Some(source_id.to_owned()),
                lifts: None,
                retained_digest: None,
            };
            if !existing.contains(&record.id) {
                append_line(&suppressions_path(runtime_dir), &record)?;
            }
            written.push(record);
        }
        Ok(written)
    })
}

/// The owner explicitly taught `text` again (new source, receipt `receipt_id`): re-enables every
/// active record whose line equals one of its normalized lines exactly. A superstring never lifts
/// ("Never use X" does not lift a forget of "Use X"), and a record closing `receipt_id` itself is
/// never lifted by that receipt's own (possibly replayed) teach. Returns the lifted record ids.
pub fn lift_suppressions(runtime_dir: &Path, text: &str, source_id: &str, receipt_id: &str) -> Result<Vec<String>, String> {
    let lines = suppression_lines(text);
    under_receipts_lock(runtime_dir, || {
        let set = SuppressionSet::load(runtime_dir)?;
        let mut lifted = Vec::new();
        for record in set.records() {
            if record.receipt_id.as_deref() == Some(receipt_id) || !lines.contains(&record.text) {
                continue;
            }
            let lift = SuppressionRecord {
                v: 1,
                at: now_iso(),
                id: short_digest("lift_", &[record.id.as_str(), source_id]),
                kind: SUPPRESSION_LIFT.to_owned(),
                text: String::new(),
                receipt_id: None,
                source_id: Some(source_id.to_owned()),
                lifts: Some(record.id.clone()),
                retained_digest: None,
            };
            append_line(&suppressions_path(runtime_dir), &lift)?;
            lifted.push(record.id.clone());
        }
        Ok(lifted)
    })
}

/// Records that a forget or correct finished removing content from the memory repository and
/// returns the new suppression generation. Every session pinned under an older generation repins.
pub fn record_purge(runtime_dir: &Path, receipt_id: &str, source_id: &str) -> Result<String, String> {
    under_receipts_lock(runtime_dir, || {
        let record = purge_record(receipt_id, source_id, None);
        append_line(&suppressions_path(runtime_dir), &record)?;
        Ok(record.id)
    })
}

/// [`record_purge`] for one forget or correct attempt, skipped when it changed nothing since the
/// receipt's last purge: it did not commit (`committed` false) and leaves exactly the same
/// `retained` paths. A repin then would land on the same content, so retrying a retained forget
/// never repins every session again. Returns the new generation, `None` when skipped.
pub fn record_purge_if_changed(runtime_dir: &Path, receipt_id: &str, source_id: &str, committed: bool, retained: &[String]) -> Result<Option<String>, String> {
    let digest = sha256_hex(retained.join("\u{0}").as_bytes());
    under_receipts_lock(runtime_dir, || {
        if !committed {
            let all: Vec<SuppressionRecord> = read_jsonl(&suppressions_path(runtime_dir))?;
            let last = all.iter().rev().find(|record| record.kind == SUPPRESSION_PURGED && record.receipt_id.as_deref() == Some(receipt_id));
            if last.is_some_and(|record| record.retained_digest.as_deref() == Some(digest.as_str())) {
                return Ok(None);
            }
        }
        let record = purge_record(receipt_id, source_id, Some(digest));
        append_line(&suppressions_path(runtime_dir), &record)?;
        Ok(Some(record.id))
    })
}

fn purge_record(receipt_id: &str, source_id: &str, retained_digest: Option<String>) -> SuppressionRecord {
    SuppressionRecord {
        v: 1,
        at: now_iso(),
        id: format!("purge_{}", random_id()),
        kind: SUPPRESSION_PURGED.to_owned(),
        text: String::new(),
        receipt_id: Some(receipt_id.to_owned()),
        source_id: Some(source_id.to_owned()),
        lifts: None,
        retained_digest,
    }
}

/// The id of the newest `purged` line; `None` before the first forget or correct. An unreadable
/// store is an error, so a caller can fail closed.
pub fn suppression_generation(runtime_dir: &Path) -> Result<Option<String>, String> {
    let all: Vec<SuppressionRecord> = read_jsonl(&suppressions_path(runtime_dir))?;
    Ok(all.into_iter().rev().find(|record| record.kind == SUPPRESSION_PURGED).map(|record| record.id))
}

/// [`suppression_generation`] of the identity whose memory repository is `repo_dir`.
pub fn suppression_generation_for_repo(repo_dir: &Path) -> Result<Option<String>, String> {
    match repo_dir.parent() {
        Some(root) => suppression_generation(&root.join(RUNTIME_DIRNAME)),
        None => Ok(None),
    }
}

/// The do-not-record constraint for a facts, reflection or dream child; empty without lines.
pub fn render_do_not_record(lines: &[String]) -> String {
    if lines.is_empty() {
        return String::new();
    }
    let listed = lines.iter().map(|line| format!("- {line}")).collect::<Vec<_>>().join("\n");
    format!(
        "\n\nThe owner retracted the statements below (forgotten or corrected). Never record, restate or paraphrase any of them, and never record anything that implies one of them, even when the input repeats it:\n{listed}"
    )
}

/// The active (not lifted) suppression records of one identity.
#[derive(Debug, Clone, Default)]
pub struct SuppressionSet {
    records: Vec<SuppressionRecord>,
}

impl SuppressionSet {
    /// A missing file is an empty set; an unreadable one is an error, so callers fail closed.
    pub fn load(runtime_dir: &Path) -> Result<Self, String> {
        let all: Vec<SuppressionRecord> = read_jsonl(&suppressions_path(runtime_dir))?;
        let lifted: BTreeSet<String> = all.iter().filter(|record| record.kind == SUPPRESSION_LIFT).filter_map(|record| record.lifts.clone()).collect();
        // Re-normalized, so a line recorded under an older normalizer still matches.
        let records = all
            .into_iter()
            .filter(|record| record.kind != SUPPRESSION_LIFT && !lifted.contains(&record.id))
            .map(|mut record| {
                record.text = normalize_memory_text(&record.text);
                record
            })
            .filter(|record| !record.text.is_empty())
            .collect();
        Ok(Self { records })
    }

    /// The set of the identity whose memory repository is `repo_dir` (`<identity>/repo`).
    pub fn for_repo(repo_dir: &Path) -> Result<Self, String> {
        match repo_dir.parent() {
            Some(root) => Self::load(&root.join(RUNTIME_DIRNAME)),
            None => Ok(Self::default()),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub fn records(&self) -> &[SuppressionRecord] {
        &self.records
    }

    /// The record new candidate text restates (containment), if any.
    pub fn find(&self, candidate: &str) -> Option<&SuppressionRecord> {
        self.records.iter().find(|record| record.matches(candidate))
    }

    pub fn suppresses(&self, candidate: &str) -> bool {
        self.find(candidate).is_some()
    }

    /// Whether `candidate` normalizes to exactly a suppressed line.
    pub fn suppresses_exactly(&self, candidate: &str) -> bool {
        self.records.iter().any(|record| record.matches_exactly(candidate))
    }

    /// The distinct suppressed lines, for a child's do-not-record constraint.
    pub fn do_not_record(&self) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        for record in &self.records {
            if !lines.contains(&record.text) {
                lines.push(record.text.clone());
            }
        }
        lines
    }

    /// What a reflection or dream run's version of one markdown file must become so it restates
    /// no suppressed record. `base` is the file before the run (`None`: the run created it) and
    /// `existing` all the content of the whole tree before the run ([`markdown_content_lines`]).
    ///
    /// Existing memory is never deleted on containment:
    /// - a body line restating a record exactly drops;
    /// - a body line that only contains a record is kept when it is existing memory (anywhere in
    ///   the tree, so a moved line survives); a new such line in a file that existed reverts the
    ///   whole file to its base (the run may have reworded an existing line); in a new file it
    ///   drops. The caller restores an existing line the run removed elsewhere (its source file
    ///   goes back to its base, see [`Self::scrub_base`]), so a reword moved anywhere never
    ///   deletes it;
    /// - a `description` that is existing content (the file's base description, or equal to any
    ///   description or line of the tree before the run) is rewritten only on an exact match, a new
    ///   one on containment too: back to the base description unless that one restates a record
    ///   exactly, else to [`NEUTRAL_DESCRIPTION`]. Every other frontmatter byte is kept.
    pub fn scrub_tip(&self, content: &str, base: Option<&str>, existing: &BTreeSet<String>) -> TipScrub {
        if self.records.is_empty() {
            return TipScrub::Clean;
        }
        match (self.scrub_file(content, base, existing), base) {
            (Scrubbed::Edited(text), _) => TipScrub::Edited(text),
            (Scrubbed::Revert, Some(base)) => TipScrub::Reverted(self.scrub_base(base, existing)),
            // A revert needs a base; without one nothing reverts.
            (Scrubbed::Clean | Scrubbed::Revert, _) => TipScrub::Clean,
        }
    }

    /// A file's version before the run without its exact restatements (a description restating a
    /// record exactly is rewritten); `None` when the base needs no change. Every base line is
    /// existing memory, so this never reverts.
    pub fn scrub_base(&self, base: &str, existing: &BTreeSet<String>) -> Option<String> {
        match self.scrub_file(base, Some(base), existing) {
            Scrubbed::Edited(text) => Some(text),
            Scrubbed::Clean | Scrubbed::Revert => None,
        }
    }

    /// [`Self::scrub_tip`] as the file's resulting text; `None` when nothing matched.
    pub fn scrub_markdown(&self, content: &str, base: Option<&str>, existing: &BTreeSet<String>) -> Option<String> {
        match self.scrub_tip(content, base, existing) {
            TipScrub::Clean => None,
            TipScrub::Edited(text) | TipScrub::Reverted(Some(text)) => Some(text),
            TipScrub::Reverted(None) => base.map(str::to_owned),
        }
    }

    fn scrub_file(&self, content: &str, base: Option<&str>, existing: &BTreeSet<String>) -> Scrubbed {
        let (head, body) = split_frontmatter(content);
        let (base_head, base_body) = base.map(split_frontmatter).unwrap_or_default();
        let base_descriptions = description_values(base_head);
        let own = markdown_body_lines([base_body]);
        let exact = |normalized: &str| self.records.iter().any(|record| record.text == normalized);
        let mut edited = false;
        let head_lines: Vec<String> = head
            .split('\n')
            .map(|line| {
                let Some((key, value)) = line.split_once(':').filter(|(key, _)| key.trim() == "description") else { return line.to_owned() };
                let value = value.trim();
                let normalized = normalize_memory_text(value);
                let hit = if base_descriptions.contains(&value) || existing.contains(&normalized) { exact(normalized.as_str()) } else { self.suppresses(value) };
                if !hit {
                    return line.to_owned();
                }
                edited = true;
                let replacement = base_descriptions.iter().copied().find(|kept| !kept.is_empty() && !exact(normalize_memory_text(kept).as_str())).unwrap_or(NEUTRAL_DESCRIPTION);
                let ending = if line.ends_with('\r') { "\r" } else { "" };
                format!("{key}: {replacement}{ending}")
            })
            .collect();
        let mut revert = false;
        let kept: Vec<&str> = body
            .split('\n')
            .filter(|line| {
                let normalized = normalize_memory_text(line);
                if normalized.is_empty() {
                    return true;
                }
                if exact(normalized.as_str()) {
                    edited = true;
                    return false;
                }
                if !self.suppresses(line) || existing.contains(&normalized) || own.contains(&normalized) {
                    return true;
                }
                if base.is_some() {
                    revert = true;
                    return true;
                }
                edited = true;
                false
            })
            .collect();
        if revert {
            Scrubbed::Revert
        } else if edited {
            Scrubbed::Edited(format!("{}{}", head_lines.join("\n"), kept.join("\n")))
        } else {
            Scrubbed::Clean
        }
    }

    /// Transcript text without the lines restating a suppressed record (containment), for the
    /// input handed to the facts child; `None` when nothing matched.
    pub fn scrub_transcript_text(&self, text: &str) -> Option<String> {
        if self.records.is_empty() {
            return None;
        }
        let mut dropped = false;
        let kept: Vec<&str> = text
            .split('\n')
            .filter(|line| {
                let hit = !line.trim().is_empty() && self.suppresses(line);
                dropped |= hit;
                !hit
            })
            .collect();
        dropped.then(|| kept.join("\n"))
    }
}

/// What [`SuppressionSet::scrub_tip`] decided for one file version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TipScrub {
    /// Nothing restates a suppressed record.
    Clean,
    /// The version without what restates one.
    Edited(String),
    /// The file goes back to its base: `Some` is the base without its exact restatements
    /// ([`SuppressionSet::scrub_base`]), `None` the base byte for byte.
    Reverted(Option<String>),
}

/// The outcome of scrubbing one file version.
enum Scrubbed {
    Clean,
    Edited(String),
    /// A new line only contains a suppressed record in a file that existed: keep the base.
    Revert,
}

fn read_jsonl<T: DeserializeOwned>(path: &Path) -> Result<Vec<T>, String> {
    let text = match read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    Ok(text.lines().filter(|line| !line.trim().is_empty()).filter_map(|line| serde_json::from_str(line).ok()).collect())
}

fn append_line(path: &Path, value: &impl Serialize) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let mut json = serde_json::to_string(value).map_err(|error| error.to_string())?;
    json.push('\n');
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|error| error.to_string())?;
    file.write_all(json.as_bytes()).map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipt_ids_are_stable_per_source() {
        assert_eq!(owner_receipt_id("slack:C1:1.2"), owner_receipt_id("slack:C1:1.2"));
        assert_ne!(owner_receipt_id("slack:C1:1.2"), owner_receipt_id("slack:C1:1.3"));
        assert!(owner_receipt_id("x").starts_with("rcpt_"));
    }

    #[test]
    fn events_fold_into_distinct_states_and_append_once() {
        let root = tempfile::tempdir().expect("runtime");
        let store = OwnerReceiptStore::new(root.path());
        let id = owner_receipt_id("source-1");
        let mut accepted = OwnerReceiptEvent::new(&id, RECEIPT_ACCEPTED);
        accepted.source_id = Some("source-1".into());
        accepted.text = Some("Use the QA host".into());
        assert!(store.append_once(accepted.clone()).expect("accept"));
        assert!(!store.append_once(accepted).expect("repeat"));
        assert_eq!(store.view(&id).expect("read").expect("view").state, RECEIPT_ACCEPTED);
        let mut persisted = OwnerReceiptEvent::new(&id, RECEIPT_PERSISTED);
        persisted.commit = Some("abc".into());
        assert!(store.append_once(persisted).expect("persist"));
        assert_eq!(store.view(&id).expect("read").expect("view").state, RECEIPT_PERSISTED);
        for session in ["s1", "s1", "s2"] {
            let mut applied = OwnerReceiptEvent::new(&id, RECEIPT_APPLIED);
            applied.session_id = Some(session.into());
            store.append_once(applied).expect("apply");
        }
        let view = store.view(&id).expect("read").expect("view");
        assert_eq!(view.state, RECEIPT_APPLIED);
        assert_eq!(view.applied.iter().map(|mark| mark.session_id.as_str()).collect::<Vec<_>>(), ["s1", "s2"]);
        assert!(store.append_once(OwnerReceiptEvent::new(&id, RECEIPT_FORGOTTEN)).expect("forget"));
        assert!(!store.append_once(OwnerReceiptEvent::new(&id, RECEIPT_CORRECTED)).expect("closed once"));
        let view = store.view(&id).expect("read").expect("view");
        assert_eq!(view.state, RECEIPT_FORGOTTEN);
        assert_eq!(view.persisted.expect("persisted").commit, "abc");
        assert!(serde_json::to_value(store.view(&id).expect("read").expect("view")).expect("json").get("text").is_none());
    }

    #[test]
    fn normalization_and_restatement() {
        assert_eq!(normalize_memory_text("- [2026-10-25]   Use  the QA host."), "use the qa host");
        assert!(restates("use the qa remote host", "The owner said: use the QA remote host always"));
        assert!(!restates("tabs", "prefers tabs over spaces"));
        assert!(restates("tabs", "- Tabs."));
        assert!(restates_exactly("use the qa remote host", "- [2026-10-27] Use the  QA remote host."));
        assert!(!restates_exactly("use the qa remote host", "Never use the QA remote host for prod deploys."));
        assert!(!restates_exactly("", ""));
    }

    #[test]
    fn markdown_decoration_and_wrapping_never_hide_an_exact_restatement() {
        for variant in [
            "1. Use the QA remote host.",
            "2) Use the QA remote host",
            "- [ ] Use the QA remote host.",
            "* [x] Use the QA remote host.",
            "> Use the QA remote host.",
            "> - **Use the QA remote host.**",
            "## Use the QA remote host",
            "*Use the QA remote host*",
            "`Use the QA remote host`",
            "\"Use the QA remote host.\"",
            "'Use the QA remote host'",
            "Use the QA remote host?",
            "Use the QA remote host:",
        ] {
            assert!(restates_exactly("use the qa remote host", variant), "{variant}");
        }
        assert_eq!(normalize_memory_text("# Builds"), "builds");
        assert_eq!(normalize_memory_text("#hashtag stays"), "#hashtag stays");
        assert!(!restates_exactly("use the qa remote host", "> Owner said: use the QA remote host."));
    }

    #[test]
    fn suppressions_are_idempotent_scrub_bodies_and_can_be_lifted() {
        let root = tempfile::tempdir().expect("runtime");
        let first = append_suppressions(root.path(), SUPPRESSION_FORGET, "Use the QA remote host.\n\nok", None, "forget-1").expect("append");
        assert_eq!(first.len(), 1, "short lines are not suppressed");
        append_suppressions(root.path(), SUPPRESSION_FORGET, "Use the QA remote host.", None, "forget-1").expect("repeat");
        assert_eq!(read_jsonl::<SuppressionRecord>(&suppressions_path(root.path())).expect("read").len(), 1);
        let set = SuppressionSet::load(root.path()).expect("load");
        let file = "---\ndescription: use the qa remote host\n---\n# Builds\n- [2026-10-27] Use the QA remote host.\n- keep tests deterministic\n";
        let none = BTreeSet::new();
        // The description is content: a restating one is rewritten, new or unchanged.
        let clean = "---\ndescription: Memory notes\n---\n# Builds\n- keep tests deterministic\n";
        assert_eq!(set.scrub_markdown(file, None, &none).as_deref(), Some(clean));
        assert_eq!(set.scrub_markdown(file, Some(file), &markdown_content_lines([file])).as_deref(), Some(clean));
        assert!(set.scrub_markdown("- other\n", None, &none).is_none());
        assert_eq!(lift_suppressions(root.path(), "use the QA remote host", "teach-2", "rcpt_other").expect("lift").len(), 1);
        assert!(SuppressionSet::load(root.path()).expect("load").is_empty());
    }

    #[test]
    fn a_description_is_scrubbed_exactly_when_unchanged_and_on_containment_when_the_run_wrote_it() {
        let root = tempfile::tempdir().expect("runtime");
        append_suppressions(root.path(), SUPPRESSION_FORGET, "Use the QA remote host.", None, "forget-1").expect("append");
        let set = SuppressionSet::load(root.path()).expect("load");
        let none = BTreeSet::new();
        let base = "---\ndescription: Rules\nkind: notes\n---\n- keep\n";
        let existing = markdown_content_lines([base]);
        // A changed description that restates goes back to the clean base one; other keys stay.
        let tip = "---\ndescription: Rules: use the QA remote host\nkind: notes\n---\n- keep\n- also new\n";
        assert_eq!(set.scrub_markdown(tip, Some(base), &existing).as_deref(), Some("---\ndescription: Rules\nkind: notes\n---\n- keep\n- also new\n"));
        // A new file's restating description becomes the neutral one.
        let created = "---\ndescription: Always use the QA remote host\n---\n- unrelated\n";
        assert_eq!(set.scrub_markdown(created, None, &existing).as_deref(), Some("---\ndescription: Memory notes\n---\n- unrelated\n"));
        // An unchanged description that only contains the line is existing memory.
        let never = "---\ndescription: Never use the QA remote host for prod deploys\n---\n- keep\n";
        let tree = markdown_content_lines([never]);
        assert!(set.scrub_markdown(never, Some(never), &tree).is_none());
        // So is the same description in a renamed or new file (equal to a description of the tree).
        assert!(set.scrub_markdown("---\ndescription: Never use the QA remote host for prod deploys\n---\n- moved\n", None, &tree).is_none());
        // A changed description that restates goes back to an existing superstring base description.
        let changed = "---\ndescription: Never use the QA remote host for prod deploys, ever\n---\n- keep\n";
        assert_eq!(set.scrub_markdown(changed, Some(never), &tree).as_deref(), Some(never));
        assert!(set.scrub_markdown("- plain\n", None, &none).is_none());
    }

    #[test]
    fn a_superstring_is_never_scrubbed_from_existing_content_nor_lifts() {
        let root = tempfile::tempdir().expect("runtime");
        append_suppressions(root.path(), SUPPRESSION_FORGET, "Use the QA remote host.", Some("rcpt_a"), "forget-a").expect("append");
        let set = SuppressionSet::load(root.path()).expect("load");
        let base = "---\ndescription: Rules\n---\n- Never use the QA remote host for prod deploys.\n";
        let existing = markdown_content_lines([base]);
        // Existing memory keeps the superstring; a line the run added is withheld on containment
        // (the file existed, so it keeps its base version).
        let tip = format!("{base}- The owner said: use the QA remote host.\n- Use the QA remote host.\n");
        assert_eq!(set.scrub_tip(&tip, Some(base), &existing), TipScrub::Reverted(None));
        assert!(set.scrub_markdown(base, Some(base), &existing).is_none());
        assert!(set.scrub_base(base, &existing).is_none());
        assert!(!set.suppresses_exactly("Never use the QA remote host for prod deploys."));
        // Moved to a new file: still existing memory.
        let moved = "---\ndescription: Deploys\n---\n- Never use the QA remote host for prod deploys.\n";
        assert!(set.scrub_markdown(moved, None, &existing).is_none());
        // Reworded in place: the file keeps its base version instead of losing the rule.
        let reworded = "---\ndescription: Rules\n---\n- Never use the QA remote host for production deploys.\n- New unrelated line.\n";
        assert_eq!(set.scrub_markdown(reworded, Some(base), &existing).as_deref(), Some(base));
        // Reworded and moved to a new file: always withheld (the finalize restores the original).
        let reworded_moved = "---\ndescription: Deploys\n---\n- Never use the QA remote host for production deploys.\n";
        assert_eq!(set.scrub_tip(reworded_moved, None, &existing), TipScrub::Edited("---\ndescription: Deploys\n---\n".into()));
        // A base with an exact restatement reverts to that base without it.
        let restating = format!("{base}- Use the QA remote host.\n");
        assert_eq!(set.scrub_base(&restating, &markdown_content_lines([restating.as_str()])).as_deref(), Some(base));
        // Teaching the superstring, or the forgotten receipt replaying its own teach, lifts nothing.
        assert!(lift_suppressions(root.path(), "Never use the QA remote host for prod deploys.", "teach-b", "rcpt_b").expect("lift").is_empty());
        assert!(lift_suppressions(root.path(), "Use the QA remote host.", "source-a", "rcpt_a").expect("lift").is_empty());
        assert_eq!(SuppressionSet::load(root.path()).expect("load").records().len(), 1);
    }

    #[test]
    fn crlf_frontmatter_is_kept_verbatim() {
        let content = "---\r\ndescription: use the qa remote host\r\n---\r\n- Use the QA remote host.\r\n- keep\r\n";
        assert_eq!(split_frontmatter(content), ("---\r\ndescription: use the qa remote host\r\n---\r\n", "- Use the QA remote host.\r\n- keep\r\n"));
        let root = tempfile::tempdir().expect("runtime");
        append_suppressions(root.path(), SUPPRESSION_FORGET, "Use the QA remote host.", None, "forget-1").expect("append");
        let set = SuppressionSet::load(root.path()).expect("load");
        let none = BTreeSet::new();
        assert_eq!(set.scrub_markdown(content, None, &none).as_deref(), Some("---\r\ndescription: Memory notes\r\n---\r\n- keep\r\n"));
    }

    #[test]
    fn purges_advance_the_generation_without_adding_a_suppression() {
        let root = tempfile::tempdir().expect("runtime");
        assert_eq!(suppression_generation(root.path()).expect("read"), None);
        append_suppressions(root.path(), SUPPRESSION_FORGET, "Use the QA remote host.", Some("rcpt_a"), "forget-a").expect("append");
        assert_eq!(suppression_generation(root.path()).expect("read"), None);
        let first = record_purge(root.path(), "rcpt_a", "forget-a").expect("purge");
        assert_eq!(suppression_generation(root.path()).expect("read").as_deref(), Some(first.as_str()));
        let second = record_purge(root.path(), "rcpt_a", "forget-a").expect("purge");
        assert_ne!(first, second);
        assert_eq!(suppression_generation(root.path()).expect("read"), Some(second));
        assert_eq!(SuppressionSet::load(root.path()).expect("load").do_not_record(), ["use the qa remote host"]);
    }

    #[test]
    fn a_purge_that_changed_nothing_since_the_last_one_is_skipped() {
        let root = tempfile::tempdir().expect("runtime");
        let retained = vec!["notes/raw.md".to_owned()];
        let first = record_purge_if_changed(root.path(), "rcpt_a", "forget-a", false, &retained).expect("purge").expect("first purge");
        assert_eq!(record_purge_if_changed(root.path(), "rcpt_a", "forget-a", false, &retained).expect("purge"), None, "same retained set, no commit");
        assert_eq!(suppression_generation(root.path()).expect("read").as_deref(), Some(first.as_str()));
        // Another receipt's purge never stands in for this one's.
        assert!(record_purge_if_changed(root.path(), "rcpt_b", "forget-b", false, &retained).expect("purge").is_some());
        let committed = record_purge_if_changed(root.path(), "rcpt_a", "forget-a", true, &retained).expect("purge");
        assert!(committed.is_some(), "a commit always purges");
        let changed = record_purge_if_changed(root.path(), "rcpt_a", "forget-a", false, &[]).expect("purge");
        assert!(changed.is_some(), "a changed retained set purges");
        assert_eq!(suppression_generation(root.path()).expect("read"), changed);
        assert!(SuppressionSet::load(root.path()).expect("load").is_empty(), "a purge is no suppression");
    }

    #[test]
    fn transcript_text_loses_restating_lines_and_the_constraint_lists_them() {
        let root = tempfile::tempdir().expect("runtime");
        append_suppressions(root.path(), SUPPRESSION_FORGET, "Use the QA remote host.", None, "forget-1").expect("append");
        let set = SuppressionSet::load(root.path()).expect("load");
        assert_eq!(set.scrub_transcript_text("Remember: use the QA remote host!\nShip on Fridays.").as_deref(), Some("Ship on Fridays."));
        assert!(set.scrub_transcript_text("Ship on Fridays.").is_none());
        assert!(render_do_not_record(&[]).is_empty());
        assert!(render_do_not_record(&set.do_not_record()).ends_with("\n- use the qa remote host"));
    }

    #[test]
    fn unreadable_suppressions_fail_closed() {
        let root = tempfile::tempdir().expect("runtime");
        std::fs::create_dir(suppressions_path(root.path())).expect("directory in place of the file");
        assert!(SuppressionSet::load(root.path()).is_err());
        assert!(suppression_generation(root.path()).is_err());
    }
}
