//! Port of senpi packages/coding-agent/src/core/credential-pool/failover.ts.
#![allow(async_fn_in_trait)]

use crate::credential_pool::classify::{CredentialAction, CredentialBlock, ClassifyContext, classify_credential_failure};
use serde_json::Value;

/// Re-exported for the lanes that still recognize the marker (the Claude SDK lane stamps it because
/// tools execute mid-stream there); the generic pool never stamps it.
pub use maho_ai::utils::provider_failure_description::TURN_RETRY_SUPPRESSION_PREFIX;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RunSlot {
    pub name: String,
    pub blocked_until: Option<u64>,
    pub block_reason: Option<String>,
    pub failure_count: Option<u32>,
    pub lease: Option<SlotLeaseRef>,
    pub pinned: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotLeaseRef {
    pub id: String,
    pub expires_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialFailoverEvent {
    pub slot_name: String,
    pub block: CredentialBlock,
    pub attempt: usize,
    pub committed_output: bool,
}

/// Thrown when an attempt failed by throwing and the pool has nothing left to try.
#[derive(Debug, Clone, PartialEq)]
pub struct CredentialFailoverError {
    pub action: CredentialAction,
    pub detail: String,
    pub retry_at: Option<u64>,
}

impl std::fmt::Display for CredentialFailoverError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail)
    }
}

impl std::error::Error for CredentialFailoverError {}

pub fn is_available(slot: &RunSlot, now: u64) -> bool {
    if matches!(slot.block_reason.as_deref(), Some("auth_error") | Some("account_disabled")) {
        return false;
    }
    if let Some(blocked_until) = slot.blocked_until
        && blocked_until > now {
            return false;
        }
    true
}

pub fn soonest_retry_at(slots: &[RunSlot], now: u64) -> Option<u64> {
    slots.iter().filter_map(|slot| slot.blocked_until).filter(|until| *until > now).min()
}

/// The outcome of one attempt: what it emitted, and how it failed when it did.
#[derive(Debug, Clone, PartialEq)]
pub struct AttemptOutcome<Event> {
    pub events: Vec<Event>,
    /// The provider's own terminal event, forwarded when the failure ends the request.
    pub terminal_event: Option<Event>,
    pub failure: Option<Value>,
}

pub trait FailoverHost {
    type Event: Clone;

    /// Re-read before every distinct-credential attempt so a newly added slot participates.
    async fn list_slots(&mut self) -> Vec<RunSlot>;
    fn select(&self, candidates: &[RunSlot]) -> Option<RunSlot>;
    /// Runs one attempt. Events are delivered to the caller as they arrive.
    async fn run_attempt(&mut self, slot: &RunSlot) -> AttemptOutcome<Self::Event>;
    /// REQUIRED and default-DENY by contract: false only for pre-commit bookkeeping.
    fn is_committed_output(&self, event: &Self::Event) -> bool;
    /// Identifies the stream-start announcement.
    fn is_stream_start(&self, _event: &Self::Event) -> bool {
        false
    }
    /// Extracts the failure carried by a terminal error EVENT.
    fn error_from_event(&self, _event: &Self::Event) -> Option<Value> {
        None
    }
    fn classify(&self, error: &Value, failure_count: u32) -> CredentialAction {
        classify_credential_failure(error, &ClassifyContext { failure_count: Some(failure_count), ..ClassifyContext::default() })
    }
    /// Persisted BEFORE a replacement slot is selected so a crash never forgets a block.
    async fn persist_block(&mut self, slot: &RunSlot, block: &CredentialBlock);
    async fn on_rotate(&mut self, _event: CredentialFailoverEvent) {}
    async fn on_success(&mut self, _slot: &RunSlot) {}
    fn now(&self) -> u64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|duration| duration.as_millis() as u64).unwrap_or(0)
    }
    /// Delivers one event to the caller.
    fn emit(&mut self, event: Self::Event);
}

/// Generic in-lane credential failover. Runs at most one failover attempt per slot per request,
/// retries provider-scoped faults on the SAME slot up to the classifier's bound without blocking it,
/// and rotates only while no committed output has reached the caller.
pub async fn run_credential_failover<H: FailoverHost>(host: &mut H) -> Result<(), CredentialFailoverError> {
    let mut attempted: Vec<String> = Vec::new();
    let mut retries_by_slot: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    let mut last_error: Option<CredentialFailoverError> = None;
    let mut last_original: Option<Value> = None;
    let mut last_terminal_event: Option<H::Event> = None;
    let mut start_announced = false;

    loop {
        let slots = host.list_slots().await;
        let now = host.now();
        let candidates: Vec<RunSlot> = slots
            .iter()
            .filter(|slot| !attempted.contains(&slot.name) && is_available(slot, now))
            .cloned()
            .collect();
        if candidates.is_empty() {
            if let Some(terminal) = last_terminal_event.take() {
                host.emit(terminal);
                return Ok(());
            }
            let retry_at = soonest_retry_at(&slots, now);
            let (action, detail) = match (last_error, last_original) {
                (Some(error), _) => (error.action, error.detail),
                (None, Some(original)) => (CredentialAction::FailRequest, original.to_string()),
                (None, None) => (CredentialAction::FailRequest, "No credential slots available".to_owned()),
            };
            return Err(CredentialFailoverError { action, detail, retry_at });
        }
        let Some(slot) = host.select(&candidates) else {
            return Err(CredentialFailoverError {
                action: CredentialAction::FailRequest,
                detail: "credential rotation selected from an empty candidate set".to_owned(),
                retry_at: None,
            });
        };

        let mut committed_output = false;
        let mut terminal_event: Option<H::Event> = None;
        let mut attempt_stream_start = false;
        let mut forwarded_start = false;
        let outcome = host.run_attempt(&slot).await;
        for event in &outcome.events {
            if let Some(failure) = host.error_from_event(event) {
                terminal_event = Some(event.clone());
                last_original = Some(failure.clone());
                break;
            }
            if host.is_stream_start(event) {
                attempt_stream_start = true;
                if start_announced {
                    continue;
                }
                start_announced = true;
                forwarded_start = true;
            }
            committed_output = committed_output || host.is_committed_output(event);
            host.emit(event.clone());
        }
        let _ = (attempt_stream_start, forwarded_start);

        let failure = match (outcome.failure, terminal_event.as_ref()) {
            (Some(failure), _) => Some(failure),
            (None, Some(event)) => host.error_from_event(event),
            (None, None) => None,
        };
        let Some(failure) = failure else {
            host.on_success(&slot).await;
            return Ok(());
        };
        last_terminal_event = terminal_event.clone();

        let failure_count = slot.failure_count.unwrap_or(0) + retries_by_slot.get(&slot.name).copied().unwrap_or(0);
        let action = host.classify(&failure, failure_count);

        if matches!(action, CredentialAction::RetrySame { .. }) && !committed_output {
            let used = retries_by_slot.get(&slot.name).copied().unwrap_or(0) + 1;
            retries_by_slot.insert(slot.name.clone(), used);
            if used < crate::credential_pool::classify::RETRY_SAME_MAX_ATTEMPTS {
                continue;
            }
            if let Some(terminal) = terminal_event {
                host.emit(terminal);
                return Ok(());
            }
            return Err(CredentialFailoverError { action, detail: failure.to_string(), retry_at: None });
        }
        let CredentialAction::Failover { block } = action.clone() else {
            if let Some(terminal) = terminal_event {
                host.emit(terminal);
                return Ok(());
            }
            return Err(CredentialFailoverError { action, detail: failure.to_string(), retry_at: None });
        };

        host.persist_block(&slot, &block).await;
        attempted.push(slot.name.clone());
        let error = CredentialFailoverError { action, detail: failure.to_string(), retry_at: None };
        last_error = Some(error.clone());
        host.on_rotate(CredentialFailoverEvent {
            slot_name: slot.name.clone(),
            block,
            attempt: attempted.len(),
            committed_output,
        })
        .await;
        if committed_output {
            if let Some(terminal) = terminal_event {
                host.emit(terminal);
                return Ok(());
            }
            return Err(error);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credential_pool::classify::{CredentialBlock, RETRY_SAME_MAX_ATTEMPTS};
    use serde_json::json;

    #[derive(Default)]
    struct FakeHost {
        slots: Vec<RunSlot>,
        attempts: Vec<Vec<Result<Vec<&'static str>, Value>>>,
        attempt_index: usize,
        emitted: Vec<&'static str>,
        blocked: Vec<(String, CredentialBlock)>,
        rotations: usize,
        successes: Vec<String>,
        now: u64,
    }

    impl FailoverHost for FakeHost {
        type Event = &'static str;

        async fn list_slots(&mut self) -> Vec<RunSlot> {
            self.slots.clone()
        }

        fn select(&self, candidates: &[RunSlot]) -> Option<RunSlot> {
            candidates.first().cloned()
        }

        async fn run_attempt(&mut self, _slot: &RunSlot) -> AttemptOutcome<Self::Event> {
            let scripted = self.attempts.get(self.attempt_index).cloned().unwrap_or_default();
            self.attempt_index += 1;
            let mut outcome = AttemptOutcome { events: Vec::new(), terminal_event: None, failure: None };
            for step in scripted {
                match step {
                    Ok(events) => outcome.events.extend(events),
                    Err(failure) => {
                        outcome.failure = Some(failure);
                        break;
                    }
                }
            }
            outcome
        }

        fn is_committed_output(&self, event: &Self::Event) -> bool {
            *event == "delta"
        }

        async fn persist_block(&mut self, slot: &RunSlot, block: &CredentialBlock) {
            self.blocked.push((slot.name.clone(), block.clone()));
        }

        async fn on_rotate(&mut self, _event: CredentialFailoverEvent) {
            self.rotations += 1;
        }

        async fn on_success(&mut self, slot: &RunSlot) {
            self.successes.push(slot.name.clone());
        }

        fn now(&self) -> u64 {
            self.now
        }

        fn emit(&mut self, event: Self::Event) {
            self.emitted.push(event);
        }
    }

    fn slot(name: &str) -> RunSlot {
        RunSlot { name: name.to_owned(), ..RunSlot::default() }
    }

    fn ok(events: Vec<&'static str>) -> Result<Vec<&'static str>, Value> {
        Ok(events)
    }

    fn failed(error: Value) -> Result<Vec<&'static str>, Value> {
        Err(error)
    }

    #[tokio::test]
    async fn rotates_to_the_next_slot_after_an_auth_failure() {
        let mut host = FakeHost {
            slots: vec![slot("a"), slot("b")],
            attempts: vec![vec![ok(vec!["start"]), failed(json!({ "status": 401, "message": "invalid api key" }))], vec![ok(vec!["delta"])]],
            ..FakeHost::default()
        };
        run_credential_failover(&mut host).await.expect("failover");
        assert_eq!(host.emitted, vec!["start", "delta"]);
        assert_eq!(host.blocked, vec![("a".to_owned(), CredentialBlock::AuthError)]);
        assert_eq!(host.rotations, 1);
        assert_eq!(host.successes, vec!["b".to_owned()]);
    }

    #[tokio::test]
    async fn retries_the_same_slot_for_a_provider_fault_then_succeeds() {
        let mut host = FakeHost {
            slots: vec![slot("a")],
            attempts: vec![vec![ok(vec!["start"]), failed(json!({ "status": 503, "message": "overloaded" }))], vec![ok(vec!["delta"])]],
            ..FakeHost::default()
        };
        run_credential_failover(&mut host).await.expect("failover");
        assert_eq!(host.emitted, vec!["start", "delta"]);
        assert!(host.blocked.is_empty());
        assert_eq!(host.rotations, 0);
    }

    #[tokio::test]
    async fn committed_output_bars_rotation_and_reports_the_failure() {
        let mut host = FakeHost {
            slots: vec![slot("a"), slot("b")],
            attempts: vec![vec![ok(vec!["start", "delta"]), failed(json!({ "status": 401, "message": "invalid api key" }))]],
            ..FakeHost::default()
        };
        let error = run_credential_failover(&mut host).await.expect_err("error");
        assert_eq!(error.action, CredentialAction::Failover { block: CredentialBlock::AuthError });
        assert_eq!(host.emitted, vec!["start", "delta"]);
        assert_eq!(host.rotations, 1);
        assert_eq!(host.blocked.len(), 1);
        assert_eq!(host.successes, Vec::<String>::new());
    }

    #[tokio::test]
    async fn a_provider_fault_after_committed_output_is_not_retried() {
        let mut host = FakeHost {
            slots: vec![slot("a")],
            attempts: vec![vec![ok(vec!["start", "delta"]), failed(json!({ "status": 503, "message": "overloaded" }))]],
            ..FakeHost::default()
        };
        let error = run_credential_failover(&mut host).await.expect_err("error");
        assert_eq!(error.action, CredentialAction::RetrySame { max_attempts: RETRY_SAME_MAX_ATTEMPTS });
        assert_eq!(host.emitted, vec!["start", "delta"]);
        assert_eq!(host.attempt_index, 1);
    }

    #[tokio::test]
    async fn a_blocked_slot_is_skipped_and_an_empty_pool_reports_the_soonest_retry() {
        let mut blocked = slot("a");
        blocked.blocked_until = Some(500);
        let mut host = FakeHost { slots: vec![blocked], now: 100, ..FakeHost::default() };
        let error = run_credential_failover(&mut host).await.expect_err("error");
        assert_eq!(error.retry_at, Some(500));
        assert_eq!(error.detail, "No credential slots available");
    }

    #[tokio::test]
    async fn availability_follows_permanent_blocks_and_deadlines() {
        let mut permanent = slot("a");
        permanent.block_reason = Some("auth_error".to_owned());
        assert!(!is_available(&permanent, 0));
        let mut cooling = slot("b");
        cooling.blocked_until = Some(100);
        assert!(!is_available(&cooling, 50));
        assert!(is_available(&cooling, 100));
        assert_eq!(soonest_retry_at(&[permanent, cooling], 10), Some(100));
    }
}