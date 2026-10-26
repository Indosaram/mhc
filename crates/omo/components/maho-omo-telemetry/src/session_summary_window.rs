//! Port of `packages/omo-senpi/src/components/telemetry/session-summary-window.ts`.
//!
//! A window sums the assistant turns of one session between two checkpoints. Cost is summed in integer
//! `cost_1e4` units so a summary and the sum of its raw turns agree exactly in any summation order, and
//! `cost_usd` is derived from the integer rather than summed.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

pub const MODEL_SLOTS: usize = 3;

#[derive(Debug, Clone, PartialEq)]
pub struct SummaryTurn {
    pub turn_index: f64,
    pub provider: String,
    pub model_id: String,
    pub input_tokens: f64,
    pub output_tokens: f64,
    pub cache_read_tokens: f64,
    pub cache_write_tokens: f64,
    pub reasoning_tokens: f64,
    pub total_tokens: f64,
    /// Cost in integer units of 1e-4 USD; the turn handler rounds once, everything downstream adds integers.
    pub cost_1e4: f64,
    pub usage_valid: bool,
    pub stop_reason: Option<String>,
    pub tool_result_count: f64,
    pub tool_error_count: f64,
}

/// Each summed property and the turn field it adds up.
const SUMMED: [(&str, TurnField); 9] = [
    ("input_tokens", TurnField::InputTokens),
    ("output_tokens", TurnField::OutputTokens),
    ("cache_read_tokens", TurnField::CacheReadTokens),
    ("cache_write_tokens", TurnField::CacheWriteTokens),
    ("reasoning_tokens", TurnField::ReasoningTokens),
    ("total_tokens", TurnField::TotalTokens),
    ("cost_1e4", TurnField::Cost1e4),
    ("tool_result_count", TurnField::ToolResultCount),
    ("tool_error_count", TurnField::ToolErrorCount),
];

#[derive(Clone, Copy)]
enum TurnField {
    InputTokens,
    OutputTokens,
    CacheReadTokens,
    CacheWriteTokens,
    ReasoningTokens,
    TotalTokens,
    Cost1e4,
    ToolResultCount,
    ToolErrorCount,
}

impl SummaryTurn {
    fn field(&self, field: TurnField) -> f64 {
        match field {
            TurnField::InputTokens => self.input_tokens,
            TurnField::OutputTokens => self.output_tokens,
            TurnField::CacheReadTokens => self.cache_read_tokens,
            TurnField::CacheWriteTokens => self.cache_write_tokens,
            TurnField::ReasoningTokens => self.reasoning_tokens,
            TurnField::TotalTokens => self.total_tokens,
            TurnField::Cost1e4 => self.cost_1e4,
            TurnField::ToolResultCount => self.tool_result_count,
            TurnField::ToolErrorCount => self.tool_error_count,
        }
    }
}

const COUNTED: [(&str, CountedField); 4] = [
    ("invalid_usage_turns", CountedField::InvalidUsage),
    ("error_turns", CountedField::Error),
    ("aborted_turns", CountedField::Aborted),
    ("custom_model_turns", CountedField::CustomModel),
];

#[derive(Clone, Copy)]
enum CountedField {
    InvalidUsage,
    Error,
    Aborted,
    CustomModel,
}

impl SummaryTurn {
    fn counts(&self, field: CountedField) -> bool {
        match field {
            CountedField::InvalidUsage => !self.usage_valid,
            CountedField::Error => self.stop_reason.as_deref() == Some("error"),
            CountedField::Aborted => self.stop_reason.as_deref() == Some("aborted"),
            CountedField::CustomModel => self.model_id == "custom",
        }
    }
}

const PER_MODEL: [(&str, TurnField); 4] = [
    ("input_tokens", TurnField::InputTokens),
    ("output_tokens", TurnField::OutputTokens),
    ("total_tokens", TurnField::TotalTokens),
    ("cost_1e4", TurnField::Cost1e4),
];

/// The numbers the schema publishes; per-model slots publish cost in USD only.
pub fn summary_number_keys() -> Vec<&'static str> {
    let mut keys = vec![
        "summary_seq",
        "sample_rate",
        "turns",
        "first_turn_index",
        "last_turn_index",
        "window_ms",
        "total_tokens_p50",
        "total_tokens_p95",
        "total_tokens_max",
        "model_count",
        "other_turns",
        "other_total_tokens",
        "other_cost_usd",
        "cost_usd",
        "dropped_events",
    ];
    keys.extend(SUMMED.iter().map(|(key, _)| *key));
    keys.extend(COUNTED.iter().map(|(key, _)| *key));
    keys
}

pub const MODEL_SLOT_NUMBER_KEYS: [&str; 5] = [
    "turns",
    "input_tokens",
    "output_tokens",
    "total_tokens",
    "cost_usd",
];

#[derive(Debug, Clone, PartialEq)]
pub struct ModelTotals {
    pub provider: String,
    pub model: String,
    pub sums: BTreeMap<String, f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SummaryWindow {
    pub started_at: f64,
    pub last_at: f64,
    pub first_turn_index: f64,
    pub last_turn_index: f64,
    pub sums: BTreeMap<String, f64>,
    pub totals: Vec<f64>,
    pub models: Vec<ModelTotals>,
}

pub const ENDED_BY: [&str; 4] = ["checkpoint", "shutdown", "reload", "recovered"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndedBy {
    Checkpoint,
    Shutdown,
    Reload,
    Recovered,
}

impl EndedBy {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Checkpoint => "checkpoint",
            Self::Shutdown => "shutdown",
            Self::Reload => "reload",
            Self::Recovered => "recovered",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SummaryMeta {
    pub ended_by: EndedBy,
    pub epoch: String,
    pub seq: u64,
    pub sample_rate: f64,
    pub sampled: bool,
    pub session_role: String,
    pub dropped_events: u64,
}

/// A run of one session's numbering: random per process (and per opt-in), kept while that process continues
/// the same window file and replaced whenever another process continues or recovers it.
pub fn new_summary_epoch() -> String {
    let mut bytes = [0_u8; 8];
    if getrandom::fill(&mut bytes).is_err() {
        let fallback = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
            .to_le_bytes();
        bytes.copy_from_slice(&fallback[..8]);
    }
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn is_summary_epoch(value: &str) -> bool {
    value.len() == 16
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub fn open_window(at: f64, turn_index: f64) -> SummaryWindow {
    SummaryWindow {
        started_at: at,
        last_at: at,
        first_turn_index: turn_index,
        last_turn_index: turn_index,
        sums: BTreeMap::new(),
        totals: Vec::new(),
        models: Vec::new(),
    }
}

fn add(sums: &mut BTreeMap<String, f64>, key: &str, value: f64) {
    *sums.entry(key.to_string()).or_insert(0.0) += value;
}

pub fn add_turn(window: &mut SummaryWindow, turn: &SummaryTurn, at: f64) {
    window.last_at = at;
    window.last_turn_index = turn.turn_index;
    window.totals.push(turn.total_tokens);
    for (key, field) in SUMMED {
        add(&mut window.sums, key, turn.field(field));
    }
    for (key, field) in COUNTED {
        if turn.counts(field) {
            add(&mut window.sums, key, 1.0);
        }
    }
    let index = window
        .models
        .iter()
        .position(|model| model.provider == turn.provider && model.model == turn.model_id);
    let index = match index {
        Some(index) => index,
        None => {
            window.models.push(ModelTotals {
                provider: turn.provider.clone(),
                model: turn.model_id.clone(),
                sums: BTreeMap::new(),
            });
            window.models.len() - 1
        }
    };
    let sums = &mut window.models[index].sums;
    add(sums, "turns", 1.0);
    for (key, field) in PER_MODEL {
        add(sums, key, turn.field(field));
    }
}

/// The published `session_summary` properties for one closed window.
pub fn summary_properties(session_hash: &str, window: &SummaryWindow, meta: &SummaryMeta) -> Value {
    let mut ranked: Vec<&ModelTotals> = window.models.iter().collect();
    ranked.sort_by(|left, right| {
        let turns = right.sums.get("turns").copied().unwrap_or(0.0);
        let left_turns = left.sums.get("turns").copied().unwrap_or(0.0);
        turns
            .total_cmp(&left_turns)
            .then_with(|| {
                right
                    .sums
                    .get("total_tokens")
                    .copied()
                    .unwrap_or(0.0)
                    .total_cmp(&left.sums.get("total_tokens").copied().unwrap_or(0.0))
            })
    });
    let rest = ranked.get(MODEL_SLOTS..).unwrap_or_default();
    let rest_sum = |key: &str| -> f64 {
        rest.iter()
            .map(|model| model.sums.get(key).copied().unwrap_or(0.0))
            .sum()
    };
    let mut properties = Map::new();
    properties.insert("$session_id".into(), json!(session_hash));
    properties.insert("summary_epoch".into(), json!(meta.epoch));
    properties.insert("summary_seq".into(), json!(meta.seq));
    properties.insert("is_final".into(), json!(meta.ended_by == EndedBy::Shutdown));
    properties.insert("ended_by".into(), json!(meta.ended_by.as_str()));
    properties.insert("session_role".into(), json!(meta.session_role));
    properties.insert("sample_rate".into(), json!(meta.sample_rate));
    properties.insert("raw_turns_sampled".into(), json!(meta.sampled));
    properties.insert("turns".into(), json!(window.totals.len()));
    properties.insert("first_turn_index".into(), json!(window.first_turn_index));
    properties.insert("last_turn_index".into(), json!(window.last_turn_index));
    properties.insert("window_ms".into(), json!(window.last_at - window.started_at));
    properties.insert("total_tokens_p50".into(), json!(percentile(&window.totals, 0.5)));
    properties.insert("total_tokens_p95".into(), json!(percentile(&window.totals, 0.95)));
    properties.insert(
        "total_tokens_max".into(),
        json!(window
            .totals
            .iter()
            .copied()
            .fold(0.0_f64, f64::max)
            .max(0.0)),
    );
    properties.insert("model_count".into(), json!(window.models.len()));
    properties.insert("other_turns".into(), json!(rest_sum("turns")));
    properties.insert("other_total_tokens".into(), json!(rest_sum("total_tokens")));
    properties.insert("other_cost_usd".into(), json!(usd_of(rest_sum("cost_1e4"))));
    properties.insert("dropped_events".into(), json!(meta.dropped_events));
    for (key, _) in SUMMED {
        properties.insert(key.into(), json!(window.sums.get(key).copied().unwrap_or(0.0)));
    }
    for (key, _) in COUNTED {
        properties.insert(key.into(), json!(window.sums.get(key).copied().unwrap_or(0.0)));
    }
    properties.insert(
        "cost_usd".into(),
        json!(usd_of(window.sums.get("cost_1e4").copied().unwrap_or(0.0))),
    );
    for slot in 1..=MODEL_SLOTS {
        let model = ranked.get(slot - 1);
        properties.insert(
            format!("m{slot}_provider"),
            json!(model.map_or("none", |model| model.provider.as_str())),
        );
        properties.insert(
            format!("m{slot}_model"),
            json!(model.map_or("none", |model| model.model.as_str())),
        );
        for key in MODEL_SLOT_NUMBER_KEYS {
            if key == "cost_usd" {
                continue;
            }
            properties.insert(
                format!("m{slot}_{key}"),
                json!(model.and_then(|model| model.sums.get(key).copied()).unwrap_or(0.0)),
            );
        }
        properties.insert(
            format!("m{slot}_cost_usd"),
            json!(usd_of(
                model
                    .and_then(|model| model.sums.get("cost_1e4").copied())
                    .unwrap_or(0.0)
            )),
        );
    }
    Value::Object(properties)
}

pub fn usd_of(cost_1e4: f64) -> f64 {
    cost_1e4 / 10_000.0
}

pub fn percentile(values: &[f64], q: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let rank = (q * sorted.len() as f64).ceil() as i64 - 1;
    let index = rank.clamp(0, sorted.len() as i64 - 1) as usize;
    sorted.get(index).copied().unwrap_or(0.0)
}

pub fn window_to_json(window: &SummaryWindow) -> Value {
    json!({
        "startedAt": window.started_at,
        "lastAt": window.last_at,
        "firstTurnIndex": window.first_turn_index,
        "lastTurnIndex": window.last_turn_index,
        "sums": window.sums,
        "totals": window.totals,
        "models": window.models.iter().map(|model| json!({
            "provider": model.provider,
            "model": model.model,
            "sums": model.sums,
        })).collect::<Vec<_>>(),
    })
}

/// A window read back from disk is untrusted: an older build, a torn copy or a hand edit must be rejected
/// rather than summed into a row that claims exact totals.
pub fn parse_window(value: &Value) -> Option<SummaryWindow> {
    let number = |key: &str| value.get(key).and_then(Value::as_f64).filter(|n| n.is_finite());
    let sums = |raw: Option<&Value>| -> Option<BTreeMap<String, f64>> {
        let object = raw?.as_object()?;
        let mut parsed = BTreeMap::new();
        for (key, entry) in object {
            parsed.insert(
                key.clone(),
                entry.as_f64().filter(|number| number.is_finite())?,
            );
        }
        Some(parsed)
    };
    let totals: Option<Vec<f64>> = value
        .get("totals")?
        .as_array()?
        .iter()
        .map(|entry| entry.as_f64().filter(|number| number.is_finite()))
        .collect();
    let mut models = Vec::new();
    for model in value.get("models")?.as_array()? {
        models.push(ModelTotals {
            provider: model.get("provider")?.as_str()?.to_string(),
            model: model.get("model")?.as_str()?.to_string(),
            sums: sums(model.get("sums"))?,
        });
    }
    Some(SummaryWindow {
        started_at: number("startedAt")?,
        last_at: number("lastAt")?,
        first_turn_index: number("firstTurnIndex")?,
        last_turn_index: number("lastTurnIndex")?,
        sums: sums(value.get("sums"))?,
        totals: totals?,
        models,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(index: u64, provider: &str, model: &str, total: f64, cost_1e4: f64) -> SummaryTurn {
        SummaryTurn {
            turn_index: index as f64,
            provider: provider.into(),
            model_id: model.into(),
            input_tokens: 2.0,
            output_tokens: 3.0,
            cache_read_tokens: 1.0,
            cache_write_tokens: 1.0,
            reasoning_tokens: 0.0,
            total_tokens: total,
            cost_1e4,
            usage_valid: true,
            stop_reason: None,
            tool_result_count: 0.0,
            tool_error_count: 0.0,
        }
    }

    fn meta(ended_by: EndedBy, seq: u64, role: &str) -> SummaryMeta {
        SummaryMeta {
            ended_by,
            epoch: "0123456789abcdef".into(),
            seq,
            sample_rate: 0.05,
            sampled: false,
            session_role: role.into(),
            dropped_events: 0,
        }
    }

    #[test]
    fn windows_aggregate_integers_and_publish_exact_usd() {
        let mut window = open_window(1000.0, 1.0);
        add_turn(&mut window, &turn(1, "openai", "gpt-5.6-sol", 10.0, 1.0), 1500.0);
        add_turn(&mut window, &turn(2, "openai", "gpt-5.6-sol", 90.0, 1.0), 2000.0);
        add_turn(&mut window, &turn(3, "anthropic", "claude-x", 20.0, 2.0), 2500.0);
        let props = summary_properties("hash", &window, &meta(EndedBy::Shutdown, 4, "root"));
        assert_eq!(props["turns"], 3);
        assert_eq!(props["cost_1e4"], 4.0);
        assert_eq!(props["cost_usd"], 0.0004);
        assert_eq!(props["total_tokens_p50"], 20.0);
        assert_eq!(props["total_tokens_p95"], 90.0);
        assert_eq!(props["total_tokens_max"], 90.0);
        assert_eq!(props["window_ms"], 1500.0);
        assert_eq!(props["model_count"], 2);
        assert_eq!(props["m1_model"], "gpt-5.6-sol");
        assert_eq!(props["m1_turns"], 2.0);
        assert_eq!(props["m1_cost_usd"], 0.0002);
        assert_eq!(props["m2_model"], "claude-x");
        assert_eq!(props["m3_provider"], "none");
        assert_eq!(props["m3_cost_usd"], 0.0);
        assert_eq!(props["is_final"], true);
        assert_eq!(props["ended_by"], "shutdown");
        assert_eq!(props["session_role"], "root");
        assert_eq!(props["summary_seq"], 4);
        assert_eq!(props["summary_epoch"], "0123456789abcdef");
    }

    #[test]
    fn summation_order_never_changes_the_integer_total() {
        let costs = [1.0, 2.0, 3.0, 4.0, 5.0];
        let mut forward = open_window(0.0, 0.0);
        for (index, cost) in costs.iter().enumerate() {
            add_turn(&mut forward, &turn(index as u64, "openai", "m", 1.0, *cost), 0.0);
        }
        let mut backward = open_window(0.0, 0.0);
        for (index, cost) in costs.iter().enumerate().rev() {
            add_turn(&mut backward, &turn(index as u64, "openai", "m", 1.0, *cost), 0.0);
        }
        let forward = summary_properties("h", &forward, &meta(EndedBy::Checkpoint, 0, "root"));
        let backward = summary_properties("h", &backward, &meta(EndedBy::Checkpoint, 0, "root"));
        assert_eq!(forward["cost_1e4"], 15.0);
        assert_eq!(forward["cost_1e4"], backward["cost_1e4"]);
        assert_eq!(forward["cost_usd"], backward["cost_usd"]);
    }

    #[test]
    fn counted_turns_and_the_rest_fold() {
        let mut window = open_window(0.0, 0.0);
        let mut failed = turn(1, "openai", "custom", 1.0, 0.0);
        failed.stop_reason = Some("error".into());
        failed.usage_valid = false;
        add_turn(&mut window, &failed, 0.0);
        let mut aborted = turn(2, "openai", "custom", 1.0, 0.0);
        aborted.stop_reason = Some("aborted".into());
        add_turn(&mut window, &aborted, 0.0);
        for slot in 0..4 {
            add_turn(&mut window, &turn(10 + slot, "p", &format!("m{slot}"), 1.0, 1.0), 0.0);
        }
        let props = summary_properties("h", &window, &meta(EndedBy::Checkpoint, 0, "root"));
        assert_eq!(props["invalid_usage_turns"], 1.0);
        assert_eq!(props["error_turns"], 1.0);
        assert_eq!(props["aborted_turns"], 1.0);
        assert_eq!(props["custom_model_turns"], 2.0);
        assert_eq!(props["model_count"], 5);
        // session-summary-window.ts keeps the top three models, leaving two one-turn models.
        assert_eq!(props["other_turns"], 2.0);
        assert_eq!(props["other_cost_usd"], 0.0002);
    }

    #[test]
    fn round_trip_keeps_totals_and_rejects_junk() {
        let mut window = open_window(10.0, 1.0);
        add_turn(&mut window, &turn(1, "openai", "m", 5.0, 3.0), 20.0);
        let parsed = parse_window(&window_to_json(&window)).expect("round trip");
        assert_eq!(parsed, window);
        for invalid in [
            json!(null),
            json!({}),
            json!({"startedAt":0,"lastAt":0,"firstTurnIndex":0,"lastTurnIndex":0,"sums":[],"totals":[],"models":[]}),
            json!({"startedAt":0,"lastAt":0,"firstTurnIndex":0,"lastTurnIndex":0,"sums":{},"totals":["x"],"models":[]}),
            json!({"startedAt":0,"lastAt":0,"firstTurnIndex":0,"lastTurnIndex":0,"sums":{},"totals":[],"models":[{}]}),
        ] {
            assert!(parse_window(&invalid).is_none(), "{invalid}");
        }
    }

    #[test]
    fn epochs_are_hex_and_open_windows_start_empty() {
        let epoch = new_summary_epoch();
        assert!(is_summary_epoch(&epoch), "{epoch}");
        assert!(!is_summary_epoch("0123456789ABCDEF"));
        assert!(!is_summary_epoch("0123456789abcde"));
        let window = open_window(7.0, 3.0);
        assert_eq!(window.started_at, 7.0);
        assert_eq!(window.last_at, 7.0);
        assert_eq!(window.first_turn_index, 3.0);
        assert_eq!(window.last_turn_index, 3.0);
        assert!(window.sums.is_empty());
        assert!(window.totals.is_empty());
        assert!(window.models.is_empty());
    }

    #[test]
    fn quantiles_are_nearest_rank_and_empty_is_zero() {
        assert_eq!(percentile(&[], 0.5), 0.0);
        assert_eq!(percentile(&[10.0], 0.5), 10.0);
        assert_eq!(percentile(&[90.0, 10.0, 20.0], 0.5), 20.0);
        assert_eq!(percentile(&[90.0, 10.0, 20.0], 0.95), 90.0);
    }
}
