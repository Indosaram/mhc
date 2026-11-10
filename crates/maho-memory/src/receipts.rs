//! The receipt journal: an append-only record of what memory maintenance did.
//!
//! Behaviour spec: `docs/cleanroom/specs/memory-receipts.md`. Receipts live in
//! `<runtime>/receipts.jsonl`, outside the memory repository, because they describe runs rather than
//! memory content: a receipt must survive the repo being reset or re-imported.
use std::path::{Path, PathBuf};

use crate::redact::redact_secret_like_material;

/// What produced a receipt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReceiptKind {
    /// A reflection or dream run.
    Run {
        /// `reflection` or `dream`.
        kind: String,
        run_id: String,
        trigger: String,
        /// How many times this run has executed.
        generation: u64,
    },
    Facts {
        /// Always `facts`.
        kind: String,
        batch_id: String,
        trigger: String,
    },
}

impl ReceiptKind {
    /// The `kind` field both variants carry.
    pub fn label(&self) -> &str {
        match self {
            Self::Run { kind, .. } | Self::Facts { kind, .. } => kind,
        }
    }
}

/// What happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiptEvent {
    Started,
    Committed,
    NoChange,
    RolledBack,
    Failed,
}

impl ReceiptEvent {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Committed => "committed",
            Self::NoChange => "no-change",
            Self::RolledBack => "rolled-back",
            Self::Failed => "failed",
        }
    }

    /// Whether this event closes its run. A closed run's receipt is appended once; an open run's
    /// can be appended again on a later reconciliation.
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Started)
    }
}

/// One receipt as stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Receipt {
    pub v: u32,
    pub at: String,
    pub host: String,
    pub pid: u32,
    pub kind: ReceiptKind,
    pub event: ReceiptEvent,
    pub sha: Option<String>,
    pub reason: Option<String>,
    /// Redacted and bounded to [`DETAIL_LIMIT`].
    pub detail: Option<String>,
}

/// Longest `detail`/`reason` kept, in UTF-16 code units, before the ellipsis.
pub const DETAIL_LIMIT: usize = 400;

fn bound(text: &str) -> String {
    let masked = redact_secret_like_material(text);
    if masked.encode_utf16().count() <= DETAIL_LIMIT {
        return masked;
    }
    let mut kept = String::with_capacity(DETAIL_LIMIT);
    let mut units = 0usize;
    for ch in masked.chars() {
        let width = ch.len_utf16();
        if units + width > DETAIL_LIMIT - 3 {
            break;
        }
        kept.push(ch);
        units += width;
    }
    kept.push_str("...");
    kept
}

impl Receipt {
    pub fn new(kind: ReceiptKind, event: ReceiptEvent, at: &str, host: &str, pid: u32) -> Self {
        Self {
            v: 1,
            at: at.to_owned(),
            host: host.to_owned(),
            pid,
            kind,
            event,
            sha: None,
            reason: None,
            detail: None,
        }
    }

    pub fn with_sha(mut self, sha: impl Into<String>) -> Self {
        self.sha = Some(sha.into());
        self
    }

    pub fn with_reason(mut self, reason: &str) -> Self {
        self.reason = Some(bound(reason));
        self
    }

    pub fn with_detail(mut self, detail: &str) -> Self {
        self.detail = Some(bound(detail));
        self
    }

    /// The key that makes two writings of the same event the same receipt.
    ///
    /// A run carries its generation, so a retried run produces a second receipt for the same event
    /// while a re-observed one does not. A facts batch has no generation, so its identity is just
    /// kind, batch and event.
    pub fn identity(&self) -> String {
        match &self.kind {
            ReceiptKind::Run {
                kind,
                run_id,
                generation,
                ..
            } => format!(
                "{kind}\u{1f}{run_id}\u{1f}{}\u{1f}{generation}",
                self.event.as_str()
            ),
            ReceiptKind::Facts { kind, batch_id, .. } => {
                format!("{kind}\u{1f}{batch_id}\u{1f}{}", self.event.as_str())
            }
        }
    }

    fn to_json(&self) -> serde_json::Value {
        let mut value = serde_json::Map::new();
        value.insert("v".to_owned(), self.v.into());
        value.insert("at".to_owned(), self.at.clone().into());
        value.insert("host".to_owned(), self.host.clone().into());
        value.insert("pid".to_owned(), self.pid.into());
        value.insert("kind".to_owned(), self.kind.label().into());
        value.insert("event".to_owned(), self.event.as_str().into());
        match &self.kind {
            ReceiptKind::Run {
                run_id,
                trigger,
                generation,
                ..
            } => {
                value.insert("runId".to_owned(), run_id.clone().into());
                value.insert("trigger".to_owned(), trigger.clone().into());
                value.insert("generation".to_owned(), generation.to_string().into());
            }
            ReceiptKind::Facts {
                batch_id, trigger, ..
            } => {
                value.insert("batchId".to_owned(), batch_id.clone().into());
                value.insert("trigger".to_owned(), trigger.clone().into());
            }
        }
        for (key, field) in [
            ("sha", &self.sha),
            ("reason", &self.reason),
            ("detail", &self.detail),
        ] {
            if let Some(text) = field {
                value.insert(key.to_owned(), text.clone().into());
            }
        }
        serde_json::Value::Object(value)
    }

    pub fn to_line(&self) -> String {
        serde_json::to_string(&self.to_json()).expect("receipt serializes")
    }

    pub fn from_line(line: &str) -> Option<Self> {
        let value: serde_json::Value = serde_json::from_str(line).ok()?;
        let object = value.as_object()?;
        let kind_label = object.get("kind")?.as_str()?.to_owned();
        let event = parse_event(object.get("event")?.as_str()?)?;
        let kind = if let Some(run_id) = object.get("runId").and_then(|v| v.as_str()) {
            ReceiptKind::Run {
                kind: kind_label,
                run_id: run_id.to_owned(),
                trigger: object.get("trigger")?.as_str()?.to_owned(),
                generation: object.get("generation")?.as_str()?.parse().ok()?,
            }
        } else {
            ReceiptKind::Facts {
                kind: kind_label,
                batch_id: object.get("batchId")?.as_str()?.to_owned(),
                trigger: object.get("trigger")?.as_str()?.to_owned(),
            }
        };
        Some(Self {
            v: object.get("v")?.as_u64()? as u32,
            at: object.get("at")?.as_str()?.to_owned(),
            host: object.get("host")?.as_str()?.to_owned(),
            pid: object.get("pid")?.as_u64()? as u32,
            kind,
            event,
            sha: text_field(object, "sha"),
            reason: text_field(object, "reason"),
            detail: text_field(object, "detail"),
        })
    }
}

fn text_field(object: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<String> {
    object.get(key)?.as_str().map(str::to_owned)
}

fn parse_event(raw: &str) -> Option<ReceiptEvent> {
    match raw {
        "started" => Some(ReceiptEvent::Started),
        "committed" => Some(ReceiptEvent::Committed),
        "no-change" => Some(ReceiptEvent::NoChange),
        "rolled-back" => Some(ReceiptEvent::RolledBack),
        "failed" => Some(ReceiptEvent::Failed),
        _ => None,
    }
}

/// What one read of the journal found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReceiptLog {
    /// Oldest first, in file order.
    pub receipts: Vec<Receipt>,
    /// Lines that did not parse. A torn tail line counts here.
    pub skipped: usize,
}

impl ReceiptLog {
    pub fn parse(source: &str) -> Self {
        let mut receipts = Vec::new();
        let mut skipped = 0;
        for line in source.split('\n') {
            if line.trim().is_empty() {
                continue;
            }
            match Receipt::from_line(line) {
                Some(receipt) => receipts.push(receipt),
                None => skipped += 1,
            }
        }
        Self { receipts, skipped }
    }

    pub fn contains(&self, identity: &str) -> bool {
        self.receipts
            .iter()
            .any(|receipt| receipt.identity() == identity)
    }

    pub fn newest(&self, kind: Option<&str>, limit: Option<usize>) -> Vec<&Receipt> {
        let mut matching: Vec<&Receipt> = self
            .receipts
            .iter()
            .filter(|receipt| kind.is_none_or(|kind| receipt.kind.label() == kind))
            .collect();
        matching.reverse();
        if let Some(limit) = limit {
            matching.truncate(limit);
        }
        matching
    }

    /// Every terminal event of a given kind, oldest first — what a reconciler re-offers.
    pub fn terminal_events(&self, kind: &str) -> Vec<&Receipt> {
        self.receipts
            .iter()
            .filter(|receipt| receipt.kind.label() == kind && receipt.event.is_terminal())
            .collect()
    }
}

pub fn receipts_path(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join("receipts.jsonl")
}

/// A missing file is a fresh install, not an error; refusing to start would make the first run
/// impossible.
pub fn read_journal(runtime_dir: &Path) -> std::io::Result<ReceiptLog> {
    match std::fs::read_to_string(receipts_path(runtime_dir)) {
        Ok(text) => Ok(ReceiptLog::parse(&text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(ReceiptLog::default()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(event: ReceiptEvent, generation: u64) -> Receipt {
        Receipt::new(
            ReceiptKind::Run {
                kind: "reflection".to_owned(),
                run_id: "run-1".to_owned(),
                trigger: "interval".to_owned(),
                generation,
            },
            event,
            "2026-11-10T00:00:00Z",
            "host",
            42,
        )
    }

    fn facts(event: ReceiptEvent) -> Receipt {
        Receipt::new(
            ReceiptKind::Facts {
                kind: "facts".to_owned(),
                batch_id: "batch-1".to_owned(),
                trigger: "interval".to_owned(),
            },
            event,
            "2026-11-10T00:00:00Z",
            "host",
            42,
        )
    }

    #[test]
    fn a_receipt_round_trips_through_its_line() {
        let receipt = run(ReceiptEvent::Committed, 1)
            .with_sha("abc123")
            .with_reason("all gates passed");
        assert_eq!(Receipt::from_line(&receipt.to_line()), Some(receipt));
    }

    #[test]
    fn a_facts_receipt_round_trips_through_its_line() {
        let receipt = facts(ReceiptEvent::NoChange);
        assert_eq!(Receipt::from_line(&receipt.to_line()), Some(receipt));
    }

    #[test]
    fn a_run_identity_includes_the_generation_and_an_event_does_not() {
        assert_ne!(
            run(ReceiptEvent::Committed, 1).identity(),
            run(ReceiptEvent::Committed, 2).identity()
        );
        assert_ne!(
            run(ReceiptEvent::Started, 1).identity(),
            run(ReceiptEvent::Committed, 1).identity()
        );
    }

    #[test]
    fn a_facts_identity_ignores_the_trigger() {
        let mut other = facts(ReceiptEvent::Committed);
        other.kind = ReceiptKind::Facts {
            kind: "facts".to_owned(),
            batch_id: "batch-1".to_owned(),
            trigger: "manual".to_owned(),
        };
        assert_eq!(facts(ReceiptEvent::Committed).identity(), other.identity());
    }

    #[test]
    fn detail_is_redacted_before_it_is_stored() {
        let receipt = run(ReceiptEvent::Failed, 1)
            .with_detail("failed with ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa expired");
        let detail = receipt.detail.expect("detail");
        assert!(!detail.contains("ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"));
        assert!(detail.contains("***"));
    }

    #[test]
    fn a_long_detail_is_bounded_and_ends_with_an_ellipsis() {
        let receipt = run(ReceiptEvent::Failed, 1).with_detail(&"a".repeat(1000));
        let detail = receipt.detail.expect("detail");
        assert_eq!(detail.encode_utf16().count(), DETAIL_LIMIT);
        assert!(detail.ends_with("..."));
    }

    #[test]
    fn a_detail_exactly_at_the_limit_is_kept_whole() {
        let receipt = run(ReceiptEvent::Failed, 1).with_detail(&"a".repeat(DETAIL_LIMIT));
        assert_eq!(
            receipt.detail.expect("detail").encode_utf16().count(),
            DETAIL_LIMIT
        );
    }

    #[test]
    fn bounding_never_splits_a_character() {
        let receipt = run(ReceiptEvent::Failed, 1).with_detail(&"가".repeat(1000));
        let detail = receipt.detail.expect("detail");
        assert!(!detail.contains('\u{fffd}'));
        assert!(detail.ends_with("..."));
    }

    #[test]
    fn bounding_never_splits_a_surrogate_pair() {
        let receipt = run(ReceiptEvent::Failed, 1).with_detail(&"🚀".repeat(500));
        let detail = receipt.detail.expect("detail");
        assert!(!detail.contains('\u{fffd}'));
        assert!(detail.ends_with("..."));
        assert!(detail.encode_utf16().count() <= DETAIL_LIMIT);
    }

    #[test]
    fn a_detail_of_one_emoji_repeated_fits_whole_when_short() {
        let receipt = run(ReceiptEvent::Failed, 1).with_detail("🚀🚀");
        assert_eq!(receipt.detail.as_deref(), Some("🚀🚀"));
    }

    #[test]
    fn a_log_counts_a_torn_tail_line_instead_of_failing() {
        let mut text = run(ReceiptEvent::Committed, 1).to_line();
        text.push('\n');
        text.push_str("{\"v\":1,\"kind\":\"reflec");
        let log = ReceiptLog::parse(&text);
        assert_eq!(log.receipts.len(), 1);
        assert_eq!(log.skipped, 1);
    }

    #[test]
    fn a_log_skips_a_line_with_an_unknown_event() {
        let text = "{\"v\":1,\"at\":\"t\",\"host\":\"h\",\"pid\":1,\"kind\":\"facts\",\"batchId\":\"b\",\"trigger\":\"x\",\"event\":\"teleported\"}";
        let log = ReceiptLog::parse(text);
        assert!(log.receipts.is_empty());
        assert_eq!(log.skipped, 1);
    }

    #[test]
    fn a_line_that_is_not_json_is_skipped_not_fatal() {
        let log = ReceiptLog::parse("not json\n");
        assert!(log.receipts.is_empty());
        assert_eq!(log.skipped, 1);
    }

    #[test]
    fn contains_finds_an_already_recorded_event() {
        let receipt = run(ReceiptEvent::Committed, 1);
        let log = ReceiptLog::parse(&format!("{}\n", receipt.to_line()));
        assert!(log.contains(&receipt.identity()));
    }

    #[test]
    fn contains_does_not_match_a_different_generation() {
        let log = ReceiptLog::parse(&format!("{}\n", run(ReceiptEvent::Committed, 1).to_line()));
        assert!(!log.contains(&run(ReceiptEvent::Committed, 2).identity()));
    }

    #[test]
    fn newest_returns_newest_first_and_honours_the_limit() {
        let mut text = String::new();
        for generation in 1..=3 {
            text.push_str(&run(ReceiptEvent::Committed, generation).to_line());
            text.push('\n');
        }
        let log = ReceiptLog::parse(&text);
        let newest = log.newest(None, Some(2));
        assert_eq!(newest.len(), 2);
        assert_eq!(
            newest[0].kind,
            ReceiptKind::Run {
                kind: "reflection".to_owned(),
                run_id: "run-1".to_owned(),
                trigger: "interval".to_owned(),
                generation: 3,
            }
        );
    }

    #[test]
    fn newest_filters_by_kind() {
        let mut text = String::new();
        text.push_str(&run(ReceiptEvent::Committed, 1).to_line());
        text.push('\n');
        text.push_str(&facts(ReceiptEvent::Committed).to_line());
        text.push('\n');
        let log = ReceiptLog::parse(&text);
        assert_eq!(log.newest(Some("facts"), None).len(), 1);
        assert_eq!(log.newest(Some("reflection"), None).len(), 1);
        assert_eq!(log.newest(Some("dream"), None).len(), 0);
    }

    #[test]
    fn terminal_events_exclude_a_started_run() {
        let mut text = String::new();
        for event in [
            ReceiptEvent::Started,
            ReceiptEvent::RolledBack,
            ReceiptEvent::Committed,
        ] {
            text.push_str(&run(event, 1).to_line());
            text.push('\n');
        }
        let log = ReceiptLog::parse(&text);
        assert_eq!(log.terminal_events("reflection").len(), 2);
    }

    #[test]
    fn only_started_is_non_terminal() {
        assert!(!ReceiptEvent::Started.is_terminal());
        for event in [
            ReceiptEvent::Committed,
            ReceiptEvent::NoChange,
            ReceiptEvent::RolledBack,
            ReceiptEvent::Failed,
        ] {
            assert!(event.is_terminal(), "{event:?}");
        }
    }

    #[test]
    fn a_missing_journal_reads_as_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = read_journal(dir.path()).expect("read");
        assert!(log.receipts.is_empty());
        assert_eq!(log.skipped, 0);
    }

    #[test]
    fn a_written_journal_reads_back() {
        let dir = tempfile::tempdir().expect("tempdir");
        let receipt = facts(ReceiptEvent::Committed);
        std::fs::write(
            receipts_path(dir.path()),
            format!("{}\n", receipt.to_line()),
        )
        .expect("write");
        let log = read_journal(dir.path()).expect("read");
        assert_eq!(log.receipts, vec![receipt]);
    }

    #[test]
    fn a_receipt_line_is_one_line_with_the_documented_fields() {
        let receipt = run(ReceiptEvent::Committed, 3).with_sha("deadbeef");
        let line = receipt.to_line();
        assert!(!line.contains('\n'));
        let value: serde_json::Value = serde_json::from_str(&line).expect("json");
        assert_eq!(value["v"], 1);
        assert_eq!(value["kind"], "reflection");
        assert_eq!(value["event"], "committed");
        assert_eq!(value["runId"], "run-1");
        assert_eq!(value["generation"], "3");
        assert_eq!(value["sha"], "deadbeef");
        assert!(value.get("batchId").is_none());
    }
}
