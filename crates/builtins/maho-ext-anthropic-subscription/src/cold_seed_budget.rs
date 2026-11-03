//! Port of senpi `anthropic-subscription/cold-seed-budget.ts` (pin 903b8336).
use crate::prompt_directive_dedupe::serialized_payload_bytes;
use maho_ai::types::{AssistantMessage, StopReason};
use maho_ai::utils::diagnostics::{AssistantMessageDiagnostic, now_ms};
use maho_ai::utils::overflow::is_context_overflow;
use regex::Regex;
use serde_json::{Map, Value, json};
use std::sync::{Mutex, OnceLock, PoisonError};

/// Marks an assistant turn that failed because a cold-seed (flatten/bootstrap) request did not fit
/// the model window. The compaction lane policy reads this marker to let senpi own the recovery.
pub const COLD_SEED_OVERFLOW_DIAGNOSTIC: &str = "claude_sdk_oauth_cold_seed_overflow";

/// Claude's tokenizer spends at least one token per ~4 UTF-8 bytes, so bytes/4 under-counts.
const UTF8_BYTES_PER_TOKEN_FLOOR: usize = 4;
/// Calibration ratios above this cap were not about the payload.
const MAX_CALIBRATION_RATIO: f64 = 8.0;
const OWN_REFUSAL_PREFIX: &str = "The conversation is too long to resend";
/// Sessions whose calibration a long-lived host keeps; the oldest-touched is dropped past it.
const MAX_CALIBRATED_SESSIONS: usize = 256;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ColdSeedOverflowDetails {
    pub estimated_tokens: Option<u64>,
    pub reported_tokens: Option<u64>,
    pub reported_limit: Option<u64>,
}

impl ColdSeedOverflowDetails {
    fn to_map(&self) -> Map<String, Value> {
        let mut map = Map::new();
        if let Some(value) = self.estimated_tokens {
            map.insert("estimatedTokens".into(), json!(value));
        }
        if let Some(value) = self.reported_tokens {
            map.insert("reportedTokens".into(), json!(value));
        }
        if let Some(value) = self.reported_limit {
            map.insert("reportedLimit".into(), json!(value));
        }
        map
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColdSeedOverflowError {
    pub estimated_tokens: u64,
    pub context_window: u64,
}

impl ColdSeedOverflowError {
    pub const NAME: &'static str = "ColdSeedOverflowError";
    pub fn new(estimated_tokens: u64, context_window: u64) -> Self {
        Self {
            estimated_tokens,
            context_window,
        }
    }
}

impl std::fmt::Display for ColdSeedOverflowError {
    // maho-ai's overflow patterns match this wording, so overflow recovery takes it like an API rejection.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{OWN_REFUSAL_PREFIX} (about {} tokens, limit {}). Compacting it and retrying.",
            self.estimated_tokens, self.context_window
        )
    }
}

impl std::error::Error for ColdSeedOverflowError {}

/// `estimateColdSeedTokens`: system prompt, tool schemas and the re-sent blocks at bytes/4.
pub fn estimate_cold_seed_tokens(
    system_prompt: Option<&str>,
    tools: Option<&Value>,
    blocks: &[Value],
) -> u64 {
    let tools = tools.map_or_else(|| "[]".to_owned(), |tools| tools.to_string());
    let fixed_bytes = system_prompt.unwrap_or_default().len() + tools.len();
    (fixed_bytes + serialized_payload_bytes(blocks)).div_ceil(UTF8_BYTES_PER_TOKEN_FLOOR) as u64
}

fn calibrations() -> &'static Mutex<Vec<(String, f64)>> {
    static MAP: OnceLock<Mutex<Vec<(String, f64)>>> = OnceLock::new();
    MAP.get_or_init(|| Mutex::new(Vec::new()))
}

/// Tokens the API counts per token bytes/4 estimated, learned from this session's rejected
/// cold-seeds; 1 until one is seen.
pub fn cold_seed_calibration(session_id: Option<&str>) -> f64 {
    let Some(session_id) = session_id else {
        return 1.0;
    };
    calibrations()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .find(|(id, _)| id == session_id)
        .map_or(1.0, |(_, ratio)| *ratio)
}

fn raise_calibration(session_id: &str, estimated_tokens: u64, reported_tokens: u64) {
    if estimated_tokens == 0 || reported_tokens == 0 {
        return;
    }
    let ratio = MAX_CALIBRATION_RATIO.min(reported_tokens as f64 / estimated_tokens as f64);
    if ratio <= cold_seed_calibration(Some(session_id)) {
        return;
    }
    let mut map = calibrations()
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    map.retain(|(id, _)| id != session_id);
    map.push((session_id.to_owned(), ratio));
    if map.len() > MAX_CALIBRATED_SESSIONS {
        map.remove(0);
    }
}

/// A restarted process re-learns from the newest cold-seed overflow marker on the branch that
/// carries both an estimate and a provider-reported count. `branch` entries are session entries
/// (`{type, message:{role, diagnostics}}`).
pub fn restore_cold_seed_calibration(session_id: &str, branch: &[Value]) {
    for entry in branch.iter().rev() {
        if entry["type"] != "message" || entry["message"]["role"] != "assistant" {
            continue;
        }
        let marker = entry["message"]["diagnostics"]
            .as_array()
            .and_then(|items| {
                items
                    .iter()
                    .find(|item| item["type"] == COLD_SEED_OVERFLOW_DIAGNOSTIC)
            });
        let Some(details) = marker.map(|marker| &marker["details"]) else {
            continue;
        };
        let (Some(estimated), Some(reported)) = (
            details["estimatedTokens"].as_u64(),
            details["reportedTokens"].as_u64(),
        ) else {
            continue;
        };
        raise_calibration(session_id, estimated, reported);
        return;
    }
}

pub fn forget_cold_seed_calibration(session_id: Option<&str>) {
    let mut map = calibrations()
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    match session_id {
        None => map.clear(),
        Some(session_id) => map.retain(|(id, _)| id != session_id),
    }
}

fn overflow_patterns() -> &'static [Regex; 3] {
    static PATTERNS: OnceLock<[Regex; 3]> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        [
            Regex::new(r"(?i)~?(\d[\d,]*)\s+tokens?\s*\(limit\s+(\d[\d,]*)\)").expect("pattern"),
            Regex::new(r"(?i)(\d[\d,]*)\s+tokens?\s*>\s*(\d[\d,]*)\s+maximum").expect("pattern"),
            Regex::new(r"(?i)about\s+(\d[\d,]*)\s+tokens?,\s*limit\s+(\d[\d,]*)").expect("pattern"),
        ]
    })
}

/// Reads the counts a provider rejection reports, in each wording the lane sees.
pub fn parse_reported_overflow_tokens(error_message: Option<&str>) -> Option<(u64, u64)> {
    let message = error_message.filter(|message| !message.is_empty())?;
    let captures = overflow_patterns()
        .iter()
        .find_map(|pattern| pattern.captures(message))?;
    let number = |index: usize| captures[index].replace(',', "").parse::<u64>().ok();
    let (tokens, limit) = (number(1)?, number(2)?);
    (tokens > 0 && limit > 0).then_some((tokens, limit))
}

/// `coldSeedOverflow`: the refusal for a cold-seed that cannot fit the calibrated window.
pub fn cold_seed_overflow(
    context_window: u64,
    estimated_tokens: u64,
    calibration: f64,
) -> Option<ColdSeedOverflowError> {
    if context_window == 0 {
        return None;
    }
    let calibrated = (estimated_tokens as f64 * calibration.max(1.0)).ceil() as u64;
    (calibrated > context_window).then(|| ColdSeedOverflowError::new(calibrated, context_window))
}

pub fn mark_cold_seed_overflow(
    output: &mut AssistantMessage,
    context_window: u64,
    cold_seed_attempt: bool,
    estimated_tokens: Option<u64>,
    session_id: Option<&str>,
) {
    if !cold_seed_attempt
        || output.stop_reason != StopReason::Error
        || !is_context_overflow(output, Some(context_window))
    {
        return;
    }
    // The lane's own refusal reports its calibrated estimate, not a provider count.
    let own_refusal = output
        .error_message
        .as_deref()
        .is_some_and(|message| message.starts_with(OWN_REFUSAL_PREFIX));
    let reported = if own_refusal {
        None
    } else {
        parse_reported_overflow_tokens(output.error_message.as_deref())
    };
    let details = ColdSeedOverflowDetails {
        estimated_tokens,
        reported_tokens: reported.map(|(tokens, _)| tokens),
        reported_limit: reported.map(|(_, limit)| limit),
    };
    if let (Some(session_id), Some((tokens, _)), Some(estimated)) =
        (session_id, reported, estimated_tokens)
    {
        raise_calibration(session_id, estimated, tokens);
    }
    output
        .diagnostics
        .get_or_insert_with(Vec::new)
        .push(AssistantMessageDiagnostic {
            kind: COLD_SEED_OVERFLOW_DIAGNOSTIC.into(),
            timestamp: now_ms(),
            error: None,
            details: Some(details.to_map()),
        });
}

pub fn is_cold_seed_overflow_message(message: &AssistantMessage) -> bool {
    message
        .diagnostics
        .as_deref()
        .unwrap_or_default()
        .iter()
        .any(|diagnostic| diagnostic.kind == COLD_SEED_OVERFLOW_DIAGNOSTIC)
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_ai::providers::faux::{FauxAssistantMessageOptions, faux_assistant_message};

    const WINDOW: u64 = 200_000;
    /// The calibration map is process-wide; the bounded-eviction case must not race the others.
    static CALIBRATION_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn serial() -> std::sync::MutexGuard<'static, ()> {
        CALIBRATION_LOCK
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn rejected(error: &str) -> AssistantMessage {
        faux_assistant_message(
            "",
            FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::Error),
                error_message: Some(error.into()),
                ..Default::default()
            },
        )
    }

    fn details(message: &AssistantMessage) -> Map<String, Value> {
        message.diagnostics.as_ref().unwrap()[0]
            .details
            .clone()
            .unwrap()
    }

    #[test]
    fn counts_utf8_bytes_so_cjk_is_not_under_counted() {
        let hangul = estimate_cold_seed_tokens(
            None,
            None,
            &[json!({"type":"text","text":"\u{AC00}".repeat(4_000)})],
        );
        let ascii = estimate_cold_seed_tokens(
            None,
            None,
            &[json!({"type":"text","text":"a".repeat(4_000)})],
        );
        assert!(hangul as f64 > ascii as f64 * 2.9);
    }

    #[test]
    fn counts_system_prompt_and_tool_schemas() {
        let blocks = [json!({"type":"text","text":"history"})];
        let bare = estimate_cold_seed_tokens(None, None, &blocks);
        let tools = json!([{"name":"read","description":"d".repeat(4_000),"parameters":{}}]);
        let dressed =
            estimate_cold_seed_tokens(Some("s".repeat(4_000).as_str()), Some(&tools), &blocks);
        assert!(dressed - bare >= 2_000);
    }

    #[test]
    fn reads_reported_counts_in_each_wording() {
        assert_eq!(
            parse_reported_overflow_tokens(Some(
                "Prompt is too long \u{b7} the request is ~1119185 tokens (limit 1000000) but this conversation is only ~659662 tokens"
            )),
            Some((1_119_185, 1_000_000))
        );
        assert_eq!(
            parse_reported_overflow_tokens(Some(
                "prompt is too long: 213,462 tokens > 200,000 maximum"
            )),
            Some((213_462, 200_000))
        );
        assert_eq!(
            parse_reported_overflow_tokens(Some(
                "The conversation is too long to resend (about 1200 tokens, limit 1000). Compacting it and retrying."
            )),
            Some((1_200, 1_000))
        );
        assert_eq!(
            parse_reported_overflow_tokens(Some("Prompt is too long (invalid_request)")),
            None
        );
        assert_eq!(parse_reported_overflow_tokens(None), None);
    }

    #[test]
    fn learns_how_far_bytes_under_counted_and_sizes_the_next_resend() {
        let _serial = serial();
        let session = "cold-seed-budget-learn";
        assert_eq!(cold_seed_calibration(Some(session)), 1.0);
        assert!(cold_seed_overflow(WINDOW, 150_000, 1.0).is_none());

        let mut bare = rejected("Prompt is too long (invalid_request)");
        mark_cold_seed_overflow(&mut bare, WINDOW, true, Some(150_000), Some(session));
        assert_eq!(
            bare.diagnostics.as_ref().unwrap()[0].kind,
            COLD_SEED_OVERFLOW_DIAGNOSTIC
        );
        assert_eq!(
            Value::Object(details(&bare)),
            json!({"estimatedTokens":150_000})
        );
        assert_eq!(cold_seed_calibration(Some(session)), 1.0);

        let mut counted = rejected(
            "Prompt is too long \u{b7} the request is ~300000 tokens (limit 200000) but this conversation",
        );
        mark_cold_seed_overflow(&mut counted, WINDOW, true, Some(150_000), Some(session));
        assert_eq!(
            Value::Object(details(&counted)),
            json!({"estimatedTokens":150_000,"reportedTokens":300_000,"reportedLimit":200_000})
        );
        assert_eq!(cold_seed_calibration(Some(session)), 2.0);
        assert_eq!(cold_seed_calibration(Some("another-session")), 1.0);
        assert_eq!(
            cold_seed_overflow(WINDOW, 150_000, cold_seed_calibration(Some(session)))
                .unwrap()
                .to_string(),
            "The conversation is too long to resend (about 300000 tokens, limit 200000). Compacting it and retrying."
        );
        assert!(cold_seed_overflow(WINDOW, 90_000, cold_seed_calibration(Some(session))).is_none());

        let mut lower = rejected(
            "Prompt is too long \u{b7} the request is ~210000 tokens (limit 200000) but this conversation",
        );
        mark_cold_seed_overflow(&mut lower, WINDOW, true, Some(150_000), Some(session));
        assert_eq!(cold_seed_calibration(Some(session)), 2.0);

        let mut not_payload =
            rejected("Prompt is too long \u{b7} the request is ~9000000 tokens (limit 200000)");
        mark_cold_seed_overflow(&mut not_payload, WINDOW, true, Some(150_000), Some(session));
        assert_eq!(cold_seed_calibration(Some(session)), 8.0);

        forget_cold_seed_calibration(Some(session));
        assert_eq!(cold_seed_calibration(Some(session)), 1.0);
    }

    #[test]
    fn never_learns_from_its_own_refusal() {
        let _serial = serial();
        let session = "cold-seed-budget-own";
        let mut counted = rejected("prompt is too long: 300000 tokens > 200000 maximum");
        mark_cold_seed_overflow(&mut counted, WINDOW, true, Some(150_000), Some(session));
        assert_eq!(cold_seed_calibration(Some(session)), 2.0);
        let refusal =
            cold_seed_overflow(WINDOW, 100_001, cold_seed_calibration(Some(session))).unwrap();
        assert_eq!(
            refusal.to_string(),
            "The conversation is too long to resend (about 200002 tokens, limit 200000). Compacting it and retrying."
        );
        let mut own = rejected(&refusal.to_string());
        mark_cold_seed_overflow(&mut own, WINDOW, true, Some(90_000), Some(session));
        assert_eq!(cold_seed_calibration(Some(session)), 2.0);
        assert_eq!(
            Value::Object(details(&own)),
            json!({"estimatedTokens":90_000})
        );
        forget_cold_seed_calibration(Some(session));
    }

    #[test]
    fn restores_calibration_from_newest_counted_marker() {
        let _serial = serial();
        let session = "cold-seed-budget-restore";
        let marked = |details: Value| json!({"type":"message","message":{"role":"assistant","diagnostics":[{"type":COLD_SEED_OVERFLOW_DIAGNOSTIC,"timestamp":1,"details":details}]}});
        let branch = [
            marked(
                json!({"estimatedTokens":100_000,"reportedTokens":150_000,"reportedLimit":200_000}),
            ),
            json!({"type":"compaction"}),
            marked(
                json!({"estimatedTokens":100_000,"reportedTokens":300_000,"reportedLimit":200_000}),
            ),
            marked(json!({"estimatedTokens":120_000})),
        ];
        restore_cold_seed_calibration(session, &branch);
        assert_eq!(cold_seed_calibration(Some(session)), 3.0);
        forget_cold_seed_calibration(Some(session));
        restore_cold_seed_calibration(session, &[marked(json!({"estimatedTokens":100_000}))]);
        assert_eq!(cold_seed_calibration(Some(session)), 1.0);
    }

    #[test]
    fn keeps_a_bounded_number_of_calibrated_sessions() {
        let _serial = serial();
        for index in 0..=MAX_CALIBRATED_SESSIONS {
            let mut counted = rejected("prompt is too long: 300000 tokens > 200000 maximum");
            mark_cold_seed_overflow(
                &mut counted,
                WINDOW,
                true,
                Some(150_000),
                Some(&format!("bounded-{index}")),
            );
        }
        assert_eq!(cold_seed_calibration(Some("bounded-0")), 1.0);
        assert_eq!(cold_seed_calibration(Some("bounded-1")), 2.0);
        assert_eq!(cold_seed_calibration(Some("bounded-256")), 2.0);
        for index in 0..=MAX_CALIBRATED_SESSIONS {
            forget_cold_seed_calibration(Some(&format!("bounded-{index}")));
        }
    }

    #[test]
    fn non_cold_seed_or_successful_turns_stay_unmarked() {
        let mut counted = rejected("prompt is too long: 300000 tokens > 200000 maximum");
        mark_cold_seed_overflow(&mut counted, WINDOW, false, Some(150_000), None);
        assert!(!is_cold_seed_overflow_message(&counted));
        let mut ok = faux_assistant_message("fits", FauxAssistantMessageOptions::default());
        mark_cold_seed_overflow(&mut ok, WINDOW, true, Some(1), None);
        assert!(!is_cold_seed_overflow_message(&ok));
    }
}
