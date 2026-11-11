//! The facts queue: batches of conversation material waiting to be turned into memory.
//!
//! Behaviour spec: `docs/cleanroom/specs/memory-facts-queue.md`. Batches are enqueued with a
//! message range, claimed so two consumers cannot read the same range, and retried on failure
//! with a streak that parks an entry instead of retrying it forever.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

/// A failing range is parked once its streak reaches this many failures.
pub const PARK_STREAK: u32 = 5;

/// Milliseconds in a day, for eligibility arithmetic.
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// The message range one batch covers.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchRange {
    /// First message id in the range.
    pub start_message_id: String,
    /// Last message id in the range.
    pub end_message_id: String,
    /// Line the range starts at in the snapshot.
    pub start_line: usize,
    /// Line the range ends at in the snapshot.
    pub end_snapshot_line: usize,
}

impl BatchRange {
    /// Whether the range covers any material at all.
    pub fn is_empty(&self) -> bool {
        self.end_message_id.is_empty() || self.end_snapshot_line <= self.start_line
    }
}

/// One captured entry inside a batch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchEntry {
    /// `user`, `assistant`, `tool_call`, and so on.
    pub kind: String,
    /// The text, when the entry has any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// The tool name, for tool calls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The tool arguments as text.
    #[serde(rename = "argsText", default, skip_serializing_if = "Option::is_none")]
    pub args_text: Option<String>,
    /// The tool result as text.
    #[serde(
        rename = "resultText",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub result_text: Option<String>,
    /// Whether the tool result was a success.
    #[serde(rename = "resultOk", default, skip_serializing_if = "Option::is_none")]
    pub result_ok: Option<bool>,
    /// When the entry was captured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub captured_at: Option<String>,
    /// Stable id of the source line.
    #[serde(
        rename = "source_line_id",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub source_line_id: Option<String>,
    /// Message the entry came from.
    #[serde(
        rename = "source_message_id",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub source_message_id: Option<String>,
}

/// One enqueued batch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactsBatch {
    /// Batch schema version.
    pub version: u32,
    /// The store identity the batch belongs to.
    pub identity: String,
    /// The session that produced it.
    #[serde(rename = "sessionId")]
    pub session_id: String,
    /// The conversation the range came from.
    #[serde(rename = "conversationId")]
    pub conversation_id: String,
    /// The covered range.
    pub range: BatchRange,
    /// When it was enqueued.
    #[serde(rename = "enqueuedAt")]
    pub enqueued_at: String,
    /// The captured entries, in order.
    pub entries: Vec<BatchEntry>,
}

impl FactsBatch {
    /// Parse a stored batch.
    pub fn parse(source: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(source)
    }

    /// Render a batch as the queue stores it.
    pub fn render(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self).map(|mut json| {
            json.push('\n');
            json
        })
    }

    /// Whether this batch should be written at all.
    pub fn is_worth_enqueuing(&self) -> bool {
        !self.range.is_empty() && !self.entries.is_empty()
    }

    /// The claim key for a message in this batch's conversation.
    pub fn claim_key(conversation_id: &str, message_id: &str) -> String {
        format!("{conversation_id}\u{0}{message_id}")
    }
}

/// One in-flight claim.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claim {
    /// The claim that took it.
    #[serde(rename = "claimId")]
    pub claim_id: String,
    /// When it was taken.
    #[serde(rename = "claimedAt")]
    pub claimed_at: String,
}

/// `claims.json`: which message ranges are being read right now.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claims {
    /// Claim keys mapped to their claim.
    #[serde(flatten)]
    pub entries: BTreeMap<String, Claim>,
}

impl Claims {
    /// Parse the stored claims file.
    pub fn parse(source: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(source)
    }

    /// Render the claims file.
    pub fn render(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self).map(|mut json| {
            json.push('\n');
            json
        })
    }

    /// Whether a message in a conversation is already claimed.
    pub fn contains(&self, conversation_id: &str, message_id: &str) -> bool {
        self.entries
            .contains_key(&FactsBatch::claim_key(conversation_id, message_id))
    }

    /// Take a claim unless it is already held.
    pub fn claim(&mut self, conversation_id: &str, message_id: &str, claim: Claim) -> bool {
        let key = FactsBatch::claim_key(conversation_id, message_id);
        if self.entries.contains_key(&key) {
            return false;
        }
        self.entries.insert(key, claim);
        true
    }

    /// Release every claim held by one claim id.
    pub fn release(&mut self, claim_id: &str) -> usize {
        let before = self.entries.len();
        self.entries.retain(|_, claim| claim.claim_id != claim_id);
        before - self.entries.len()
    }
}

/// The applied watermark for one conversation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsumedMark {
    /// Last applied message id.
    pub end_message_id: String,
    /// Last applied snapshot line.
    pub end_snapshot_line: usize,
    /// When it was applied.
    #[serde(rename = "consumedAt")]
    pub consumed_at: String,
}

/// `consumed.json`: how far each conversation has been applied.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Consumed {
    /// File schema version.
    pub version: u32,
    /// Per-conversation watermark.
    pub consumed: BTreeMap<String, ConsumedMark>,
}

impl Consumed {
    /// Parse a stored watermark file.
    pub fn parse(source: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(source)
    }

    /// Render the watermark file.
    pub fn render(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self).map(|mut json| {
            json.push('\n');
            json
        })
    }

    /// Whether a range ends at or before the watermark, and so is already applied.
    pub fn covers(&self, batch: &FactsBatch) -> bool {
        self.consumed
            .get(&batch.conversation_id)
            .is_some_and(|mark| {
                batch.range.end_snapshot_line <= mark.end_snapshot_line
                    || batch.range.end_message_id == mark.end_message_id
            })
    }

    /// Record a batch as applied.
    pub fn record(&mut self, batch: &FactsBatch, consumed_at: impl Into<String>) {
        self.consumed.insert(
            batch.conversation_id.clone(),
            ConsumedMark {
                end_message_id: batch.range.end_message_id.clone(),
                end_snapshot_line: batch.range.end_snapshot_line,
                consumed_at: consumed_at.into(),
            },
        );
    }
}

/// Whether a failing range is still being retried or has been parked.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FailureState {
    /// Still retried on the next attempt.
    #[serde(rename = "retrying")]
    Retrying,
    /// Not retried again automatically.
    #[serde(rename = "parked")]
    Parked,
}

/// One failing range in the ledger.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailureEntry {
    /// The conversation the range came from.
    #[serde(rename = "conversationId")]
    pub conversation_id: String,
    /// Last message id in the failing range.
    pub end_message_id: String,
    /// Last snapshot line in the failing range.
    pub end_snapshot_line: usize,
    /// Whether it is retried or parked.
    pub state: FailureState,
    /// Consecutive failures.
    pub streak: u32,
    /// First failure instant.
    #[serde(rename = "firstFailureAt")]
    pub first_failure_at: String,
    /// Most recent failure instant.
    #[serde(rename = "lastFailureAt")]
    pub last_failure_at: String,
    /// Short reason code, such as `child_exit`.
    #[serde(rename = "lastReason")]
    pub last_reason: String,
    /// Identifier of the most recent failure.
    #[serde(rename = "lastFailureId")]
    pub last_failure_id: String,
    /// When it may be retried again; `null` means never automatically.
    #[serde(rename = "nextEligibleAt", default)]
    pub next_eligible_at: Option<String>,
}

impl FailureEntry {
    /// Record one more failure on a range.
    ///
    /// The streak counts up, and the fifth consecutive failure parks the entry with no automatic
    /// retry; below that the entry stays retryable immediately.
    pub fn record_failure(
        conversation_id: &str,
        range: &BatchRange,
        failure_id: &str,
        reason: &str,
        at: &str,
    ) -> Self {
        let streak = 1;
        Self {
            conversation_id: conversation_id.to_owned(),
            end_message_id: range.end_message_id.clone(),
            end_snapshot_line: range.end_snapshot_line,
            state: FailureState::Retrying,
            streak,
            first_failure_at: at.to_owned(),
            last_failure_at: at.to_owned(),
            last_reason: reason.to_owned(),
            last_failure_id: failure_id.to_owned(),
            next_eligible_at: None,
        }
    }

    /// Number the streak, parking the entry once it reaches the threshold.
    pub fn bump(&mut self, failure_id: &str, reason: &str, at: &str) {
        self.streak = self.streak.saturating_add(1);
        self.last_failure_at = at.to_owned();
        self.last_reason = reason.to_owned();
        self.last_failure_id = failure_id.to_owned();
        if self.streak >= PARK_STREAK {
            self.state = FailureState::Parked;
            self.next_eligible_at = None;
        }
    }

    /// Whether the range may be tried again at `now_ms`.
    pub fn is_retryable_at(&self, now_ms: i64) -> bool {
        match self.state {
            FailureState::Retrying => true,
            FailureState::Parked => self
                .next_eligible_at
                .as_deref()
                .is_some_and(|at| parse_instant_ms(at).is_some_and(|at| at <= now_ms)),
        }
    }

    /// Make a parked entry eligible again after a delay.
    pub fn park_until(&mut self, at: &str, days: i64) {
        self.state = FailureState::Parked;
        let base = parse_instant_ms(at).unwrap_or(0);
        self.next_eligible_at = Some(format_ms(base.saturating_add(days.max(0) * DAY_MS)));
    }
}

/// `failures.json`: every range that recently failed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failures {
    /// File schema version.
    pub version: u32,
    /// When the ledger last changed.
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
    /// One entry per failing range.
    pub entries: Vec<FailureEntry>,
}

impl Failures {
    /// Parse a stored failure ledger.
    pub fn parse(source: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(source)
    }

    /// Render the ledger.
    pub fn render(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self).map(|mut json| {
            json.push('\n');
            json
        })
    }

    /// Find the entry for a range.
    pub fn find(
        &mut self,
        conversation_id: &str,
        end_message_id: &str,
    ) -> Option<&mut FailureEntry> {
        self.entries.iter_mut().find(|entry| {
            entry.conversation_id == conversation_id && entry.end_message_id == end_message_id
        })
    }

    /// Record a failure, bumping an existing entry for the same range.
    pub fn record(
        &mut self,
        conversation_id: &str,
        range: &BatchRange,
        failure_id: &str,
        reason: &str,
        at: &str,
    ) {
        self.updated_at = at.to_owned();
        match self.find(conversation_id, &range.end_message_id) {
            Some(entry) => entry.bump(failure_id, reason, at),
            None => self.entries.push(FailureEntry::record_failure(
                conversation_id,
                range,
                failure_id,
                reason,
                at,
            )),
        }
    }

    /// Drop an entry once its range succeeds.
    pub fn clear(&mut self, conversation_id: &str, end_message_id: &str) -> bool {
        let before = self.entries.len();
        self.entries.retain(|entry| {
            !(entry.conversation_id == conversation_id && entry.end_message_id == end_message_id)
        });
        before != self.entries.len()
    }

    /// The entries that may be retried at `now_ms`.
    pub fn retryable_at(&self, now_ms: i64) -> Vec<&FailureEntry> {
        self.entries
            .iter()
            .filter(|entry| entry.is_retryable_at(now_ms))
            .collect()
    }
}

/// Parse an RFC 3339 instant into epoch milliseconds.
fn parse_instant_ms(at: &str) -> Option<i64> {
    let (date, rest) = at.split_once('T')?;
    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: i64 = date_parts.next()?.parse().ok()?;
    let day: i64 = date_parts.next()?.parse().ok()?;
    let time = rest.trim_end_matches('Z');
    let time = time.split(['.', 'Z']).next()?;
    let mut time_parts = time.split(':');
    let hour: i64 = time_parts.next()?.parse().ok()?;
    let minute: i64 = time_parts.next()?.parse().ok()?;
    let second: i64 = time_parts.next()?.parse().ok()?;
    let millis: i64 = rest
        .split_once('.')
        .and_then(|(_, tail)| {
            let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
            digits.parse().ok()
        })
        .unwrap_or(0);

    let days = days_from_civil(year, month, day);
    Some(((days * 86_400 + hour * 3_600 + minute * 60 + second) * 1_000) + millis)
}

/// Days since the Unix epoch for a civil date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The inverse of [`days_from_civil`], as `YYYY-MM-DD`.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let days = days + 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// Render epoch milliseconds as an RFC 3339 instant with a `Z` suffix.
fn format_ms(millis: i64) -> String {
    let days = millis.div_euclid(86_400_000);
    let rest = millis.rem_euclid(86_400_000);
    let (year, month, day) = civil_from_days(days);
    let hour = rest / 3_600_000;
    let minute = (rest % 3_600_000) / 60_000;
    let second = (rest % 60_000) / 1_000;
    let millis = rest % 1_000;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

/// Why a batch could not be enqueued.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EnqueueRefusal {
    /// The batch covers no messages.
    EmptyRange,
    /// The batch has no captured entries.
    NoEntries,
    /// The conversation is already past this range.
    AlreadyConsumed {
        /// The message id the conversation already reached.
        end_message_id: String,
    },
}

impl fmt::Display for EnqueueRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyRange => f.write_str("batch range is empty"),
            Self::NoEntries => f.write_str("batch has no captured entries"),
            Self::AlreadyConsumed { end_message_id } => {
                write!(
                    f,
                    "conversation is already consumed through {end_message_id}"
                )
            }
        }
    }
}

impl std::error::Error for EnqueueRefusal {}

/// Decide whether a batch may be enqueued against the current watermark.
///
/// A refusal carries the reason; an accepted batch is safe to write.
pub fn admit_batch(batch: &FactsBatch, consumed: &Consumed) -> Result<(), EnqueueRefusal> {
    if batch.range.is_empty() {
        return Err(EnqueueRefusal::EmptyRange);
    }
    if batch.entries.is_empty() {
        return Err(EnqueueRefusal::NoEntries);
    }
    if let Some(mark) = consumed.consumed.get(&batch.conversation_id)
        && batch.range.end_snapshot_line <= mark.end_snapshot_line
    {
        return Err(EnqueueRefusal::AlreadyConsumed {
            end_message_id: mark.end_message_id.clone(),
        });
    }
    Ok(())
}

/// The context row a batch inherits: the last entry before the batch's start that is not a reasoning row.
///
/// The extractor is given this row so it can tell which message the extracted facts answer. A reasoning row is
/// skipped because it is the model thinking to itself, not something the person said or was told; using it
/// would attribute facts to a turn nobody can see. Intervening rows of other kinds are *not* skipped — the
/// nearest visible row is the real context, even when reasoning sits between it and the batch.
pub fn lead_of(entries: &[BatchEntry], start_index: usize) -> Option<BatchEntry> {
    entries
        .get(..start_index.min(entries.len()))?
        .iter()
        .rev()
        .find(|entry| entry.kind != "reasoning")
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const BATCH: &str = r#"{
  "version": 1,
  "identity": "indo-b00481cc",
  "sessionId": "01a1c486",
  "conversationId": "01a1c486",
  "range": {
    "start_message_id": "e2cff149",
    "end_message_id": "aa4387e4",
    "start_line": 0,
    "end_snapshot_line": 4
  },
  "enqueuedAt": "2026-11-10T08:16:05.773Z",
  "entries": [
    { "kind": "user", "text": "run", "captured_at": "2026-11-10T08:16:05.756Z",
      "source_line_id": "e2cff149:user", "source_message_id": "e2cff149" },
    { "kind": "tool_call", "name": "get_goal", "argsText": "{}",
      "resultText": "{\n  \"goal\": null\n}", "resultOk": true,
      "source_message_id": "f3420636" }
  ]
}"#;

    fn batch() -> FactsBatch {
        FactsBatch::parse(BATCH).expect("batch")
    }

    fn range(start_line: usize, end_line: usize, end_message_id: &str) -> BatchRange {
        BatchRange {
            start_message_id: "start".to_owned(),
            end_message_id: end_message_id.to_owned(),
            start_line,
            end_snapshot_line: end_line,
        }
    }

    #[test]
    fn a_stored_batch_parses_with_its_entries_and_range() {
        let batch = batch();
        assert_eq!(batch.conversation_id, "01a1c486");
        assert_eq!(batch.range.end_snapshot_line, 4);
        assert_eq!(batch.entries.len(), 2);
        assert_eq!(batch.entries[0].kind, "user");
        assert_eq!(batch.entries[1].name.as_deref(), Some("get_goal"));
        assert_eq!(batch.entries[1].result_ok, Some(true));
    }

    #[test]
    fn a_batch_round_trips_through_its_json() {
        let batch = batch();
        let rendered = batch.render().expect("render");
        assert_eq!(FactsBatch::parse(&rendered).expect("parse"), batch);
        assert!(rendered.contains("\"conversationId\""));
        assert!(rendered.contains("\"argsText\""));
    }

    #[test]
    fn an_empty_range_is_not_worth_enqueuing() {
        let mut batch = batch();
        batch.range.end_snapshot_line = 0;
        assert!(!batch.is_worth_enqueuing());
        assert_eq!(
            admit_batch(&batch, &Consumed::default()),
            Err(EnqueueRefusal::EmptyRange)
        );
    }

    #[test]
    fn a_batch_with_no_entries_is_refused() {
        let mut batch = batch();
        batch.entries.clear();
        assert_eq!(
            admit_batch(&batch, &Consumed::default()),
            Err(EnqueueRefusal::NoEntries)
        );
    }

    #[test]
    fn a_range_already_past_the_watermark_is_refused() {
        let mut consumed = Consumed::default();
        consumed.record(&batch(), "2026-11-10T08:16:06.000Z");
        let mut later = batch();
        later.range.end_snapshot_line = 2;
        assert_eq!(
            admit_batch(&later, &consumed),
            Err(EnqueueRefusal::AlreadyConsumed {
                end_message_id: "aa4387e4".to_owned()
            })
        );
    }

    #[test]
    fn a_later_range_is_admitted_past_the_watermark() {
        let mut consumed = Consumed::default();
        consumed.record(&batch(), "2026-11-10T08:16:06.000Z");
        let mut later = batch();
        later.range.start_line = 4;
        later.range.end_snapshot_line = 9;
        later.range.end_message_id = "bb000001".to_owned();
        assert_eq!(admit_batch(&later, &consumed), Ok(()));
    }

    #[test]
    fn a_claim_stops_a_second_consumer_from_taking_the_same_message() {
        let mut claims = Claims::default();
        let first = Claim {
            claim_id: "c1".to_owned(),
            claimed_at: "2026-11-10T08:16:05.985Z".to_owned(),
        };
        assert!(claims.claim("conv", "aa4387e4", first));
        assert!(claims.contains("conv", "aa4387e4"));
        assert!(!claims.claim(
            "conv",
            "aa4387e4",
            Claim {
                claim_id: "c2".to_owned(),
                claimed_at: "2026-11-10T08:16:06.000Z".to_owned(),
            }
        ));
    }

    #[test]
    fn releasing_a_claim_clears_every_key_it_held() {
        let mut claims = Claims::default();
        for message in ["a", "b"] {
            claims.claim(
                "conv",
                message,
                Claim {
                    claim_id: "c1".to_owned(),
                    claimed_at: "2026-11-10T08:16:05.985Z".to_owned(),
                },
            );
        }
        assert_eq!(claims.release("c1"), 2);
        assert!(claims.entries.is_empty());
    }

    #[test]
    fn claims_round_trip_through_the_stored_file() {
        let mut claims = Claims::default();
        claims.claim(
            "conv",
            "aa4387e4",
            Claim {
                claim_id: "c1".to_owned(),
                claimed_at: "2026-11-10T08:16:05.985Z".to_owned(),
            },
        );
        let rendered = claims.render().expect("render");
        assert!(rendered.contains("\"claimId\""));
        assert_eq!(Claims::parse(&rendered).expect("parse"), claims);
    }

    #[test]
    fn a_watermark_covers_ranges_ending_at_or_before_it() {
        let mut consumed = Consumed::default();
        consumed.record(&batch(), "2026-11-10T08:16:06.000Z");
        assert!(consumed.covers(&batch()));
        let mut earlier = batch();
        earlier.range.end_snapshot_line = 1;
        earlier.range.end_message_id = "00000000".to_owned();
        assert!(consumed.covers(&earlier));
    }

    #[test]
    fn a_new_range_is_not_covered_by_an_older_watermark() {
        let mut consumed = Consumed::default();
        consumed.record(&batch(), "2026-11-10T08:16:06.000Z");
        let mut later = batch();
        later.range.end_snapshot_line = 9;
        later.range.end_message_id = "bb000001".to_owned();
        assert!(!consumed.covers(&later));
    }

    #[test]
    fn a_failure_below_the_threshold_stays_retryable() {
        let entry = FailureEntry::record_failure(
            "conv",
            &range(0, 4, "aa4387e4"),
            "f1",
            "child_exit",
            "2026-11-10T08:11:11.546Z",
        );
        assert_eq!(entry.state, FailureState::Retrying);
        assert_eq!(entry.streak, 1);
        assert!(entry.is_retryable_at(0));
    }

    #[test]
    fn the_fifth_consecutive_failure_parks_the_range() {
        let mut entry = FailureEntry::record_failure(
            "conv",
            &range(0, 4, "aa4387e4"),
            "f1",
            "child_exit",
            "2026-11-10T08:11:11.546Z",
        );
        for index in 2..=PARK_STREAK {
            entry.bump(
                &format!("f{index}"),
                "child_exit",
                "2026-11-10T08:11:11.546Z",
            );
        }
        assert_eq!(entry.streak, PARK_STREAK);
        assert_eq!(entry.state, FailureState::Parked);
        assert_eq!(entry.next_eligible_at, None);
        assert!(!entry.is_retryable_at(i64::MAX));
    }

    #[test]
    fn a_parked_range_becomes_retryable_after_its_delay() {
        let mut entry = FailureEntry::record_failure(
            "conv",
            &range(0, 4, "aa4387e4"),
            "f1",
            "child_exit",
            "2026-11-10T08:11:11.546Z",
        );
        entry.park_until("2026-11-10T08:11:11.546Z", 1);
        assert_eq!(
            entry.next_eligible_at.as_deref(),
            Some("2026-11-11T08:11:11.546Z")
        );
        let now = parse_instant_ms("2026-11-11T08:11:11.546Z").expect("instant");
        assert!(entry.is_retryable_at(now));
        assert!(!entry.is_retryable_at(now - 1));
    }

    #[test]
    fn a_repeated_failure_bumps_one_ledger_entry() {
        let mut failures = Failures::default();
        failures.record(
            "conv",
            &range(0, 4, "aa4387e4"),
            "f1",
            "child_exit",
            "2026-11-10T08:11:11.546Z",
        );
        failures.record(
            "conv",
            &range(0, 4, "aa4387e4"),
            "f2",
            "child_exit",
            "2026-11-10T08:12:11.546Z",
        );
        assert_eq!(failures.entries.len(), 1);
        assert_eq!(failures.entries[0].streak, 2);
        assert_eq!(failures.entries[0].last_failure_id, "f2");
        assert_eq!(failures.updated_at, "2026-11-10T08:12:11.546Z");
    }

    #[test]
    fn a_success_clears_the_ledger_entry() {
        let mut failures = Failures::default();
        failures.record(
            "conv",
            &range(0, 4, "aa4387e4"),
            "f1",
            "child_exit",
            "2026-11-10T08:11:11.546Z",
        );
        assert!(failures.clear("conv", "aa4387e4"));
        assert!(failures.entries.is_empty());
        assert!(!failures.clear("conv", "aa4387e4"));
    }

    #[test]
    fn a_parked_entry_is_excluded_from_the_retry_list() {
        let mut failures = Failures::default();
        failures.record(
            "conv",
            &range(0, 4, "aa4387e4"),
            "f1",
            "child_exit",
            "2026-11-10T08:11:11.546Z",
        );
        failures.record(
            "conv",
            &range(5, 9, "bb000001"),
            "f2",
            "child_exit",
            "2026-11-10T08:11:11.546Z",
        );
        if let Some(entry) = failures.find("conv", "aa4387e4") {
            for _ in 0..(PARK_STREAK - 1) {
                entry.bump("f9", "child_exit", "2026-11-10T08:11:11.546Z");
            }
        }
        let retryable =
            failures.retryable_at(parse_instant_ms("2026-11-10T09:00:00.000Z").unwrap());
        assert_eq!(retryable.len(), 1);
        assert_eq!(retryable[0].end_message_id, "bb000001");
    }

    #[test]
    fn a_failure_ledger_round_trips() {
        let mut failures = Failures::default();
        failures.record(
            "conv",
            &range(0, 4, "aa4387e4"),
            "f1",
            "child_exit",
            "2026-11-10T08:11:11.546Z",
        );
        failures.entries[0].park_until("2026-11-10T08:11:11.546Z", 14);
        let rendered = failures.render().expect("render");
        assert!(rendered.contains("\"state\": \"parked\""));
        assert_eq!(Failures::parse(&rendered).expect("parse"), failures);
    }

    #[test]
    fn an_instant_parses_to_the_epoch_millisecond() {
        assert_eq!(parse_instant_ms("1970-01-01T00:00:00.000Z"), Some(0));
        assert_eq!(
            parse_instant_ms("2026-11-10T08:16:05.773Z"),
            Some(1_794_298_565_773)
        );
    }

    #[test]
    fn an_epoch_millisecond_renders_back_to_the_same_instant() {
        for at in [
            "1970-01-01T00:00:00.000Z",
            "2026-11-10T08:16:05.773Z",
            "2027-02-28T23:59:59.999Z",
        ] {
            let millis = parse_instant_ms(at).expect("instant");
            assert_eq!(format_ms(millis), at);
        }
    }

    fn row(kind: &str) -> BatchEntry {
        BatchEntry {
            kind: kind.to_owned(),
            text: Some(format!("{kind} text")),
            name: None,
            args_text: None,
            result_text: None,
            result_ok: None,
            captured_at: None,
            source_line_id: None,
            source_message_id: Some(format!("{kind}-id")),
        }
    }

    #[test]
    fn a_batch_at_the_start_has_no_lead() {
        assert_eq!(lead_of(&[], 0), None);
        assert_eq!(lead_of(&[row("assistant")], 0), None);
    }

    #[test]
    fn the_lead_is_the_entry_immediately_before_the_start() {
        let entries = vec![row("user"), row("assistant"), row("user")];
        assert_eq!(lead_of(&entries, 2), Some(row("assistant")));
    }

    #[test]
    fn reasoning_rows_are_skipped() {
        let entries = vec![row("user"), row("reasoning"), row("assistant")];
        assert_eq!(lead_of(&entries, 2), Some(row("user")));
    }

    #[test]
    fn a_visible_row_behind_reasoning_still_wins() {
        let entries = vec![row("assistant"), row("reasoning"), row("user")];
        assert_eq!(lead_of(&entries, 3), Some(row("user")));
    }

    #[test]
    fn only_reasoning_before_the_batch_means_no_lead() {
        let entries = vec![row("reasoning"), row("reasoning"), row("user")];
        assert_eq!(lead_of(&entries, 2), None);
    }

    #[test]
    fn only_entries_before_the_start_are_considered() {
        let entries = vec![row("user"), row("assistant"), row("user")];
        assert_eq!(
            lead_of(&entries, 1).expect("lead").source_message_id,
            Some("user-id".to_owned())
        );
    }

    #[test]
    fn a_start_beyond_the_end_uses_the_whole_transcript() {
        let entries = vec![row("user"), row("assistant")];
        assert_eq!(lead_of(&entries, 99), Some(row("assistant")));
    }
}
