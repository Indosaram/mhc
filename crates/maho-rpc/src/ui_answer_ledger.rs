//! Per-session receipts of answered UI requests (senpi `ui-answer-ledger.ts`): deliberately never persisted to the
//! transcript. A named answer (`answerId`) that repeats is a `replay`; any other answer to an already answered
//! request is `already_answered`.
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};

const LEDGER_CAPACITY: usize = 1024;
const SNAPSHOT_RECEIPTS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiAnswerReceipt {
    pub ui_request_id: String,
    pub answer_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LedgerLookup {
    Replay,
    AlreadyAnswered,
}

#[derive(Debug, Default)]
pub struct UiAnswerLedger {
    order: VecDeque<String>,
    answers: HashMap<String, UiAnswerReceipt>,
}

impl UiAnswerLedger {
    pub fn lookup(&self, ui_request_id: &str, answer_id: Option<&str>) -> Option<LedgerLookup> {
        let receipt = self.answers.get(ui_request_id)?;
        Some(
            if answer_id.is_some() && answer_id == receipt.answer_id.as_deref() {
                LedgerLookup::Replay
            } else {
                LedgerLookup::AlreadyAnswered
            },
        )
    }

    pub fn record(&mut self, ui_request_id: &str, answer_id: Option<&str>) {
        if self.answers.contains_key(ui_request_id) {
            return;
        }
        self.answers.insert(
            ui_request_id.to_owned(),
            UiAnswerReceipt {
                ui_request_id: ui_request_id.to_owned(),
                answer_id: answer_id.map(str::to_owned),
            },
        );
        self.order.push_back(ui_request_id.to_owned());
        if self.answers.len() > LEDGER_CAPACITY
            && let Some(oldest) = self.order.pop_front()
        {
            self.answers.remove(&oldest);
        }
    }

    /// The newest 64 receipts in answer order, for `get_state.answeredUiRequests`.
    pub fn snapshot(&self) -> Value {
        let skip = self.order.len().saturating_sub(SNAPSHOT_RECEIPTS);
        Value::Array(
            self.order
                .iter()
                .skip(skip)
                .filter_map(|id| self.answers.get(id))
                .map(|receipt| {
                    let mut value = json!({ "uiRequestId": receipt.ui_request_id });
                    if let Some(answer_id) = &receipt.answer_id {
                        value["answerId"] = answer_id.clone().into();
                    }
                    value
                })
                .collect(),
        )
    }
}

/// The ledger of one session (senpi `sessionUiAnswerLedger`, a `WeakMap` keyed by session): keyed by session id.
pub fn session_ui_answer_ledger(session_id: &str) -> Arc<Mutex<UiAnswerLedger>> {
    static LEDGERS: OnceLock<Mutex<HashMap<String, Arc<Mutex<UiAnswerLedger>>>>> = OnceLock::new();
    LEDGERS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .entry(session_id.to_owned())
        .or_default()
        .clone()
}

/// `answerId` is optional, and when present a string of 1..=256 characters (`invalid_answer_id` otherwise).
pub fn valid_answer_id(response: &Value) -> Result<Option<&str>, ()> {
    match response.get("answerId") {
        None => Ok(None),
        Some(Value::String(id)) if (1..=256).contains(&id.chars().count()) => Ok(Some(id)),
        Some(_) => Err(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_repeated_named_answer_replays_and_any_other_is_already_answered() {
        let mut ledger = UiAnswerLedger::default();
        assert_eq!(ledger.lookup("r", Some("a")), None);
        ledger.record("r", Some("a"));
        ledger.record("r", Some("b"));
        assert_eq!(ledger.lookup("r", Some("a")), Some(LedgerLookup::Replay));
        assert_eq!(
            ledger.lookup("r", Some("b")),
            Some(LedgerLookup::AlreadyAnswered)
        );
        assert_eq!(
            ledger.lookup("r", None),
            Some(LedgerLookup::AlreadyAnswered)
        );
        assert_eq!(
            ledger.snapshot(),
            json!([{"uiRequestId":"r","answerId":"a"}])
        );
    }
    #[test]
    fn the_ledger_is_bounded_and_the_snapshot_shows_the_newest() {
        let mut ledger = UiAnswerLedger::default();
        for i in 0..1100 {
            ledger.record(&format!("r{i}"), None);
        }
        assert_eq!(ledger.answers.len(), LEDGER_CAPACITY);
        assert_eq!(ledger.lookup("r0", None), None);
        let snapshot = ledger.snapshot();
        assert_eq!(snapshot.as_array().unwrap().len(), SNAPSHOT_RECEIPTS);
        assert_eq!(snapshot[63]["uiRequestId"], "r1099");
    }
    #[test]
    fn answer_ids_are_bounded_strings() {
        assert_eq!(valid_answer_id(&json!({})), Ok(None));
        assert_eq!(valid_answer_id(&json!({"answerId":"x"})), Ok(Some("x")));
        assert!(valid_answer_id(&json!({"answerId":""})).is_err());
        assert!(valid_answer_id(&json!({"answerId":"x".repeat(257)})).is_err());
        assert!(valid_answer_id(&json!({"answerId":1})).is_err());
    }
}
