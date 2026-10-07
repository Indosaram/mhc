//! Kibitzer output contract and pending-nudge handoff (latest `recall/gate.ts`).
//!
//! The resident sidecar speaks only through the nudge tool; the parent is authoritative and
//! re-validates every collected nudge against the candidate set, the session ledger, the hint shape
//! and the configured cap. Accepted nudges wait in a per-session pending file until the next turn
//! injects them.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::ledger::{sanitize_session_filename, write_private};
use crate::fs::resilient;
use crate::sync::redact::contains_secret_like_material;
use crate::support::time::{now_iso, now_millis, parse_rfc3339};

/// Pending payload schema version.
pub const PENDING_NUDGES_VERSION: u32 = 1;

/// Hint budget: one factual sentence. Internal, deliberately not a config knob.
pub const NUDGE_HINT_MAX_CHARS: usize = 200;

/// Pending payloads older than this are junk from an abandoned session.
const PENDING_TTL_MS: i64 = 24 * 60 * 60_000;

/// One accepted nudge: a stored path and the single-sentence hint about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecallNudge {
    pub path: String,
    pub hint: String,
}

/// The self-describing per-session pending payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingNudgesFile {
    pub version: u32,
    pub session_id: String,
    pub written_at: String,
    pub nudges: Vec<RecallNudge>,
}

/// Why a hint cannot be admitted, in the order the rules are checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidHintReason {
    Empty,
    TooLong,
    Multiline,
    DecisionCommentary,
    AddressesAgent,
}

/// The full nudge contract, used at admission.
pub fn describe_invalid_hint(hint: &str) -> Option<InvalidHintReason> {
    describe_hint_shape(hint).or_else(|| {
        if addresses_agent(hint) {
            Some(InvalidHintReason::AddressesAgent)
        } else {
            None
        }
    })
}

/// The REPLAY half of the contract: the hint budget only, without the nudge-only rule.
pub fn is_valid_hint(hint: &str) -> bool {
    describe_hint_shape(hint).is_none()
}

fn describe_hint_shape(hint: &str) -> Option<InvalidHintReason> {
    if hint.is_empty() {
        return Some(InvalidHintReason::Empty);
    }
    if hint.chars().count() > NUDGE_HINT_MAX_CHARS {
        return Some(InvalidHintReason::TooLong);
    }
    if hint.contains(['\r', '\n']) {
        return Some(InvalidHintReason::Multiline);
    }
    if has_decision_language(hint) {
        return Some(InvalidHintReason::DecisionCommentary);
    }
    None
}

fn addresses_agent(hint: &str) -> bool {
    let trimmed = hint.trim();
    has_second_person(trimmed)
        || has_imperative_opening(trimmed)
        || has_korean_request_ending(trimmed)
        || has_korean_prohibition(trimmed)
}

/// Alphanumeric word tokens (Unicode-aware), matching regex `\b` word boundaries closely enough
/// for the hint contract's ASCII and Hangul vocabulary.
fn words_of(text: &str) -> Vec<String> {
    let lowered = text.to_lowercase();
    let mut words = Vec::new();
    let mut current = String::new();
    for ch in lowered.chars() {
        if ch.is_alphanumeric() {
            current.push(ch);
        } else if !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

const SECOND_PERSON: [&str; 4] = ["you", "your", "yours", "yourself"];

fn has_second_person(trimmed: &str) -> bool {
    words_of(trimmed)
        .iter()
        .any(|word| SECOND_PERSON.contains(&word.as_str()))
}

const IMPERATIVE_VERBS: [&str; 19] = [
    "do not", "don't", "don’t", "never", "always", "make sure", "ensure", "verify", "check", "run",
    "use", "read", "stop", "avoid", "remember", "keep", "prefer", "skip", "consider",
];

fn has_imperative_opening(hint: &str) -> bool {
    let trimmed = hint.trim_start_matches(|ch: char| {
        ch.is_whitespace()
            || matches!(
                ch,
                '"' | '\'' | '`' | '\u{00ab}' | '\u{2018}' | '\u{2019}' | '\u{201c}' | '\u{201d}'
            )
    });
    let collapsed: String = trimmed
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    IMPERATIVE_VERBS.iter().any(|verb| {
        collapsed.strip_prefix(verb).is_some_and(|rest| {
            rest.chars().next().is_none_or(|ch| !ch.is_alphanumeric())
        })
    })
}

/// `(?:세요|십시오|십시요|하라|해라|합니다|지\s*마(?:라)?)\s*[.!]?$`.
fn has_korean_request_ending(trimmed: &str) -> bool {
    let mut text = trimmed.trim_end();
    if let Some(stripped) = text.strip_suffix(['.', '!']) {
        text = stripped.trim_end();
    }
    const SUFFIXES: [&str; 6] = ["세요", "십시오", "십시요", "하라", "해라", "합니다"];
    if SUFFIXES.iter().any(|suffix| text.ends_with(suffix)) {
        return true;
    }
    let base = text.strip_suffix('라').unwrap_or(text);
    if let Some(rest) = base.strip_suffix('마') {
        return rest.trim_end().ends_with('지');
    }
    false
}

/// `하지\s*마` anywhere in the hint.
fn has_korean_prohibition(text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    for (index, ch) in chars.iter().enumerate() {
        if *ch == '마' {
            let mut cursor = index;
            while cursor > 0 && chars[cursor - 1].is_whitespace() {
                cursor -= 1;
            }
            if cursor > 0 && chars[cursor - 1] == '지' {
                return true;
            }
        }
    }
    false
}

/// Decision commentary is not a memory fact.
fn has_decision_language(hint: &str) -> bool {
    let lower = hint.to_lowercase();
    if lower.contains("no stored memory") || lower.contains("clears the bar") {
        return true;
    }
    let words = words_of(&lower);
    for (index, word) in words.iter().enumerate() {
        if (word == "no" || word == "not") && words.get(index + 1).is_some_and(|next| next == "relevant")
        {
            return true;
        }
        if word != "memory" && word != "memories" {
            continue;
        }
        match words.get(index + 1).map(String::as_str) {
            Some("is") | Some("are") => match (
                words.get(index + 2).map(String::as_str),
                words.get(index + 3).map(String::as_str),
            ) {
                (Some("unrelated"), Some("to")) | (Some("not"), Some("about")) => return true,
                _ => {}
            },
            Some("does") | Some("do")
                if words.get(index + 2).is_some_and(|next| next == "not")
                    && matches!(
                        words.get(index + 3).map(String::as_str),
                        Some("cover") | Some("address") | Some("pertain")
                    )
                =>
            {
                return true;
            }
            Some("cover") | Some("covers")
                if words.get(index + 2..).is_some_and(|rest| {
                    rest.windows(2)
                        .any(|pair| pair[0] == "not" && pair[1] == "the")
                }) =>
            {
                return true;
            }
            _ => {}
        }
    }
    false
}

/// Options for parent-side revalidation of sidecar output.
pub struct ValidateNudgesOptions {
    /// Paths the parent offered the sidecar; anything else is fabricated.
    pub candidates: BTreeSet<String>,
    /// Paths already surfaced in this session; they never repeat.
    pub surfaced: BTreeSet<String>,
    /// Authoritative cap from config (`memory.recall.max_items`).
    pub max_items: usize,
}

/// Parent-side validation against the full admission contract; order is preserved.
pub fn validate_nudges(nudges: &[RecallNudge], options: &ValidateNudgesOptions) -> Vec<RecallNudge> {
    if options.max_items == 0 {
        return Vec::new();
    }
    let mut accepted: Vec<RecallNudge> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for nudge in nudges {
        if accepted.len() >= options.max_items {
            break;
        }
        if seen.contains(&nudge.path) {
            continue;
        }
        if !options.candidates.contains(&nudge.path) {
            continue;
        }
        if options.surfaced.contains(&nudge.path) {
            continue;
        }
        if describe_invalid_hint(&nudge.hint).is_some() {
            continue;
        }
        seen.insert(nudge.path.clone());
        accepted.push(nudge.clone());
    }
    accepted
}

/// Pending nudge handoff store: one JSON file per session under the pending directory.
pub struct PendingNudges {
    dir: PathBuf,
}

impl PendingNudges {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// Writes the session's pending payload atomically; empty input writes nothing.
    pub fn write(&self, session_id: &str, nudges: &[RecallNudge]) -> std::io::Result<()> {
        if nudges.is_empty() {
            return Ok(());
        }
        let target = self.session_file_path(session_id);
        resilient::create_dir_all(&self.dir)?;
        self.prune(&target);
        let payload = PendingNudgesFile {
            version: PENDING_NUDGES_VERSION,
            session_id: session_id.to_string(),
            written_at: now_iso(),
            nudges: nudges.to_vec(),
        };
        let temporary = temporary_path(&target);
        let body = format!("{}\n", serde_json::to_string_pretty(&payload)?);
        write_private(&temporary, body.as_bytes())?;
        resilient::rename(&temporary, &target)
    }

    /// Consumes the session's pending nudges; a foreign session or an expired payload yields none.
    pub fn take(&self, session_id: &str) -> Vec<RecallNudge> {
        let target = self.session_file_path(session_id);
        let Ok(raw) = resilient::read_to_string(&target) else {
            return Vec::new();
        };
        let Some(payload) = parse_pending_file(&raw) else {
            remove_quietly(&target);
            return Vec::new();
        };
        if payload.session_id != session_id {
            return Vec::new();
        }
        remove_quietly(&target);
        let Some(written_at) = parse_rfc3339(&payload.written_at) else {
            return Vec::new();
        };
        if now_millis() - written_at > PENDING_TTL_MS {
            return Vec::new();
        }
        payload.nudges
    }

    /// Targeted retraction of one session's payload; a mismatch leaves the file for its real owner.
    pub fn delete(&self, session_id: &str) {
        let target = self.session_file_path(session_id);
        let Ok(raw) = resilient::read_to_string(&target) else {
            return;
        };
        if let Some(payload) = parse_pending_file(&raw)
            && payload.session_id != session_id
        {
            return;
        }
        remove_quietly(&target);
    }

    fn session_file_path(&self, session_id: &str) -> PathBuf {
        self.dir
            .join(format!("{}.json", sanitize_session_filename(session_id)))
    }

    /// Best-effort sweep of abandoned sibling payloads and `.tmp-*` orphans.
    fn prune(&self, current_target: &Path) {
        let Ok(names) = resilient::read_dir_names(&self.dir) else {
            return;
        };
        let cutoff = now_millis() - PENDING_TTL_MS;
        for name in names {
            let candidate = self.dir.join(&name);
            if candidate == current_target {
                continue;
            }
            if !name.ends_with(".json") && !name.contains(".tmp-") {
                continue;
            }
            let Ok(meta) = resilient::metadata(&candidate) else {
                continue;
            };
            let mtime_ms = meta
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|elapsed| elapsed.as_secs_f64() * 1000.0)
                .unwrap_or(0.0);
            if mtime_ms as i64 > cutoff {
                continue;
            }
            remove_quietly(&candidate);
        }
    }
}

fn parse_nudge(value: &serde_json::Value) -> Option<RecallNudge> {
    let record = value.as_object()?;
    let path = record.get("path")?.as_str()?;
    let hint = record.get("hint")?.as_str()?;
    if path.is_empty() || hint.is_empty() {
        return None;
    }
    if !is_valid_hint(hint) || contains_secret_like_material(hint) {
        return None;
    }
    Some(RecallNudge {
        path: path.to_string(),
        hint: hint.to_string(),
    })
}

/// Parse the pending payload fail-closed: an unparsable or non-conforming entry drops the payload.
pub fn parse_pending_file(raw: &str) -> Option<PendingNudgesFile> {
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    let record = value.as_object()?;
    if record.get("version")?.as_u64()? != u64::from(PENDING_NUDGES_VERSION) {
        return None;
    }
    let session_id = record.get("sessionId")?.as_str()?;
    if session_id.is_empty() {
        return None;
    }
    let written_at = record.get("writtenAt")?.as_str()?.to_string();
    let entries = record.get("nudges")?.as_array()?;
    let mut nudges = Vec::new();
    for entry in entries {
        nudges.push(parse_nudge(entry)?);
    }
    Some(PendingNudgesFile {
        version: PENDING_NUDGES_VERSION,
        session_id: session_id.to_string(),
        written_at,
        nudges,
    })
}

fn temporary_path(target: &Path) -> PathBuf {
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "session".to_string());
    target.with_file_name(format!("{name}.tmp-{}", std::process::id()))
}

fn remove_quietly(path: &Path) {
    let _ = resilient::remove_file(path);
}

#[cfg(test)]
#[path = "gate_tests.rs"]
mod tests;
