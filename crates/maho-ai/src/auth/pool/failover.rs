//! Port of senpi packages/ai/src/auth/pool/failover.ts.

use crate::auth::pool::classify::{PoolBlockReason, PoolFailureAction, PoolFailureClassification, PoolFailureError, classify_pool_failure};
use crate::auth::pool::select::SelectableSlot;
use crate::utils::provider_failure_description::TURN_RETRY_SUPPRESSION_PREFIX;
use futures::stream::{Stream, StreamExt};
use std::future::Future;
use std::pin::Pin;

pub const DEFAULT_SLOT_BLOCK_MS: f64 = 60_000.0;
pub const MAX_SLOT_BLOCK_MS: f64 = 48.0 * 60.0 * 60.0 * 1_000.0;

#[derive(Debug, Clone)]
pub struct PoolFailoverEvent<TSlot> {
    pub slot: TSlot,
    pub next_slot: Option<TSlot>,
    pub classification: PoolFailureClassification,
    pub attempt: u32,
    pub committed_output: bool,
}

#[derive(Debug, thiserror::Error)]
pub struct PoolFailoverError {
    pub classification: PoolFailureClassification,
    pub detail: String,
    pub suppress_turn_retry: bool,
}

impl std::fmt::Display for PoolFailoverError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.suppress_turn_retry {
            write!(f, "{TURN_RETRY_SUPPRESSION_PREFIX}{}", self.detail)
        } else {
            write!(f, "{}", self.detail)
        }
    }
}

impl PoolFailoverError {
    pub fn new(classification: PoolFailureClassification, detail: impl Into<String>, suppress_turn_retry: bool) -> Self {
        Self { classification, detail: detail.into(), suppress_turn_retry }
    }
}

fn blocked_slot<TSlot: SelectableSlot>(
    slot: &TSlot,
    classification: &PoolFailureClassification,
    now: f64,
    attempt: u32,
    base_block_ms: f64,
) -> TSlot {
    if classification.block_reason == Some(PoolBlockReason::AuthError) {
        return slot.with_auth_block();
    }
    let fallback = MAX_SLOT_BLOCK_MS.min(base_block_ms * 2f64.powi(attempt as i32));
    let duration = MAX_SLOT_BLOCK_MS.min(classification.retry_after_ms.unwrap_or(fallback));
    slot.with_rate_limit_block(now + duration)
}

pub type SelectFn<TSlot> = Box<dyn Fn(&[TSlot]) -> TSlot + Send>;
pub type AttemptStream<TEvent> = Pin<Box<dyn Stream<Item = Result<TEvent, PoolFailureError>> + Send>>;
pub type RunAttemptFn<TSlot, TEvent> = Box<dyn Fn(&TSlot) -> AttemptStream<TEvent> + Send>;
pub type ClassifyFn = Box<dyn Fn(&PoolFailureError) -> PoolFailureClassification + Send>;
pub type CommittedFn<TEvent> = Box<dyn Fn(&TEvent) -> bool + Send>;
pub type PersistBlockFn<TSlot> = Box<dyn Fn(&TSlot) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send>;
pub type OnRotateFn<TSlot> = Box<dyn Fn(&PoolFailoverEvent<TSlot>) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send>;

pub struct PoolFailoverOptions<TSlot, TEvent> {
    pub slots: Vec<TSlot>,
    pub select: SelectFn<TSlot>,
    pub run_attempt: RunAttemptFn<TSlot, TEvent>,
    pub classify: Option<ClassifyFn>,
    pub is_committed_output: Option<CommittedFn<TEvent>>,
    pub persist_block: Option<PersistBlockFn<TSlot>>,
    pub on_rotate: Option<OnRotateFn<TSlot>>,
    pub now: Box<dyn Fn() -> f64 + Send>,
    pub base_block_ms: f64,
}

/// Runs at most one attempt per slot. A rotation is transparent only while no committed output
/// has reached the caller; afterwards the classified error is thrown with the turn-retry
/// suppression marker so the session layer never replays a partially delivered turn. Non-rotate
/// classes throw immediately and compose with the retry/model-fallback machinery above this
/// engine.
pub async fn run_slot_failover<TSlot: SelectableSlot + Clone + Send + 'static, TEvent: Send + 'static>(
    options: PoolFailoverOptions<TSlot, TEvent>,
) -> Result<Vec<TEvent>, PoolFailoverError> {
    let mut slots = options.slots.clone();
    let mut last_error: Option<PoolFailoverError> = None;
    let total_slots = options.slots.len() as u32;
    let mut collected = Vec::new();
    let is_committed = options.is_committed_output.as_ref();

    for attempt in 0..total_slots {
        let slot = (options.select)(&slots);
        let mut committed_output = false;
        let mut stream = (options.run_attempt)(&slot);
        let mut failed: Option<PoolFailureError> = None;
        loop {
            match stream.next().await {
                Some(Ok(event)) => {
                    committed_output = committed_output || is_committed.is_none_or(|committed| committed(&event));
                    collected.push(event);
                }
                Some(Err(error)) => {
                    failed = Some(error);
                    break;
                }
                None => break,
            }
        }
        let Some(error) = failed else {
            return Ok(collected);
        };
        let classification = match &options.classify {
            Some(classify) => classify(&error),
            None => classify_pool_failure(&error),
        };
        let failure = PoolFailoverError::new(classification, error.to_string(), committed_output);
        if classification.action != PoolFailureAction::Rotate {
            return Err(failure);
        }

        let now = (options.now)();
        let blocked = blocked_slot(&slot, &classification, now, attempt, options.base_block_ms);
        slots = slots.into_iter().map(|candidate| if candidate.name() == blocked.name() { blocked.clone() } else { candidate }).collect();
        if let Some(persist_block) = &options.persist_block {
            persist_block(&blocked).await;
        }
        let mut rotation = PoolFailoverEvent {
            slot: blocked,
            next_slot: None,
            classification,
            attempt: attempt + 1,
            committed_output,
        };
        if !committed_output && attempt + 1 < slots.len() as u32 {
            rotation.next_slot = Some((options.select)(&slots));
        }
        if let Some(on_rotate) = &options.on_rotate {
            on_rotate(&rotation).await;
        }
        last_error = Some(failure);
        if committed_output {
            return Err(last_error.expect("set above"));
        }
    }
    Err(last_error.unwrap_or_else(|| {
        PoolFailoverError::new(
            PoolFailureClassification { action: PoolFailureAction::Fail, block_reason: None, retry_after_ms: None },
            "Credential pool failover exhausted without an attempt",
            false,
        )
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;

    #[derive(Debug, Clone, PartialEq)]
    struct TestSlot {
        name: String,
        blocked_until: Option<f64>,
        block_reason: Option<String>,
    }

    impl SelectableSlot for TestSlot {
        fn name(&self) -> &str {
            &self.name
        }
        fn blocked_until(&self) -> Option<f64> {
            self.blocked_until
        }
        fn block_reason(&self) -> Option<&str> {
            self.block_reason.as_deref()
        }
        fn clear_block(&self) -> Self {
            Self { blocked_until: None, block_reason: None, ..self.clone() }
        }
        fn with_auth_block(&self) -> Self {
            Self { blocked_until: None, block_reason: Some("auth_error".into()), ..self.clone() }
        }
        fn with_rate_limit_block(&self, blocked_until: f64) -> Self {
            Self { blocked_until: Some(blocked_until), block_reason: Some("rate_limit".into()), ..self.clone() }
        }
    }

    fn make_slot(name: &str) -> TestSlot {
        TestSlot { name: name.into(), blocked_until: None, block_reason: None }
    }

    fn first_unblocked(slots: &[TestSlot]) -> TestSlot {
        slots
            .iter()
            .find(|slot| slot.blocked_until.is_none() && slot.block_reason.is_none())
            .cloned()
            .expect("no unblocked slot")
    }

    const NOW: f64 = 1_756_000_000_000.0;

    #[tokio::test]
    async fn succeeds_on_first_attempt_with_no_rotation() {
        let slots = vec![make_slot("a")];
        let options = PoolFailoverOptions::<TestSlot, u32> {
            slots: slots.clone(),
            select: Box::new(|slots: &[TestSlot]| slots[0].clone()),
            run_attempt: Box::new(|_slot| Box::pin(stream::iter(vec![Ok(1u32), Ok(2u32)]))),
            classify: None,
            is_committed_output: Some(Box::new(|_| true)),
            persist_block: None,
            on_rotate: None,
            now: Box::new(|| 0.0),
            base_block_ms: DEFAULT_SLOT_BLOCK_MS,
        };
        let result = run_slot_failover(options).await.unwrap();
        assert_eq!(result, vec![1, 2]);
    }

    #[tokio::test]
    async fn a_pre_output_429_rotates_to_the_next_slot_and_blocks_the_failed_one() {
        let persisted = std::sync::Arc::new(std::sync::Mutex::new(Vec::<TestSlot>::new()));
        let rotations = std::sync::Arc::new(std::sync::Mutex::new(Vec::<PoolFailoverEvent<TestSlot>>::new()));
        let attempted = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let persisted_clone = persisted.clone();
        let rotations_clone = rotations.clone();
        let attempted_clone = attempted.clone();
        let options = PoolFailoverOptions::<TestSlot, u32> {
            slots: vec![make_slot("alpha"), make_slot("beta")],
            select: Box::new(first_unblocked),
            run_attempt: Box::new(move |slot: &TestSlot| {
                attempted_clone.lock().unwrap().push(slot.name.clone());
                if slot.name == "alpha" {
                    Box::pin(stream::iter(vec![Err(PoolFailureError::from(
                        serde_json::json!({"status": 429, "message": "rate limited"}),
                    ))]))
                } else {
                    Box::pin(stream::iter(vec![Ok(1u32)]))
                }
            }),
            classify: None,
            is_committed_output: Some(Box::new(|_| true)),
            persist_block: Some(Box::new(move |slot: &TestSlot| {
                let persisted = persisted_clone.clone();
                let slot = slot.clone();
                Box::pin(async move {
                    persisted.lock().unwrap().push(slot);
                })
            })),
            on_rotate: Some(Box::new(move |event: &PoolFailoverEvent<TestSlot>| {
                let rotations = rotations_clone.clone();
                let event = event.clone();
                Box::pin(async move {
                    rotations.lock().unwrap().push(event);
                })
            })),
            now: Box::new(|| NOW),
            base_block_ms: DEFAULT_SLOT_BLOCK_MS,
        };
        let result = run_slot_failover(options).await.unwrap();
        assert_eq!(result, vec![1]);
        assert_eq!(*attempted.lock().unwrap(), vec!["alpha".to_string(), "beta".to_string()]);
        let persisted = persisted.lock().unwrap();
        assert_eq!(persisted.len(), 1);
        assert_eq!(persisted[0].name, "alpha");
        assert_eq!(persisted[0].block_reason.as_deref(), Some("rate_limit"));
        assert!(persisted[0].blocked_until.unwrap() > NOW);
        let rotations = rotations.lock().unwrap();
        assert_eq!(rotations[0].next_slot.as_ref().map(|slot| slot.name.as_str()), Some("beta"));
    }

    #[tokio::test]
    async fn default_deny_any_yielded_event_commits_the_turn_and_suppresses_rotation() {
        let attempted = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let attempted_clone = attempted.clone();
        let options = PoolFailoverOptions::<TestSlot, u32> {
            slots: vec![make_slot("alpha"), make_slot("beta")],
            select: Box::new(first_unblocked),
            run_attempt: Box::new(move |slot: &TestSlot| {
                attempted_clone.lock().unwrap().push(slot.name.clone());
                Box::pin(stream::iter(vec![
                    Ok(1u32),
                    Err(PoolFailureError::from(serde_json::json!({"status": 429, "message": "rate limited"}))),
                ]))
            }),
            classify: None,
            is_committed_output: None,
            persist_block: None,
            on_rotate: None,
            now: Box::new(|| NOW),
            base_block_ms: DEFAULT_SLOT_BLOCK_MS,
        };
        let error = run_slot_failover(options).await.unwrap_err();
        assert_eq!(*attempted.lock().unwrap(), vec!["alpha".to_string()]);
        assert!(error.suppress_turn_retry);
        assert!(error.to_string().starts_with(TURN_RETRY_SUPPRESSION_PREFIX));
    }

    #[tokio::test]
    async fn an_explicit_is_committed_output_keeps_rotation_transparent_for_bookkeeping_events() {
        let attempted = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let attempted_clone = attempted.clone();
        let options = PoolFailoverOptions::<TestSlot, u32> {
            slots: vec![make_slot("alpha"), make_slot("beta")],
            select: Box::new(first_unblocked),
            run_attempt: Box::new(move |slot: &TestSlot| {
                attempted_clone.lock().unwrap().push(slot.name.clone());
                if slot.name == "alpha" {
                    Box::pin(stream::iter(vec![Err(PoolFailureError::from(
                        serde_json::json!({"status": 429, "message": "rate limited"}),
                    ))]))
                } else {
                    Box::pin(stream::iter(vec![Ok(7u32)]))
                }
            }),
            classify: None,
            is_committed_output: Some(Box::new(|event: &u32| *event == 7)),
            persist_block: None,
            on_rotate: None,
            now: Box::new(|| NOW),
            base_block_ms: DEFAULT_SLOT_BLOCK_MS,
        };
        let result = run_slot_failover(options).await.unwrap();
        assert_eq!(*attempted.lock().unwrap(), vec!["alpha".to_string(), "beta".to_string()]);
        assert_eq!(result, vec![7]);
    }

    #[tokio::test]
    async fn an_auth_failure_blocks_the_slot_without_an_expiry() {
        let persisted = std::sync::Arc::new(std::sync::Mutex::new(Vec::<TestSlot>::new()));
        let persisted_clone = persisted.clone();
        let options = PoolFailoverOptions::<TestSlot, u32> {
            slots: vec![make_slot("alpha"), make_slot("beta")],
            select: Box::new(first_unblocked),
            run_attempt: Box::new(|slot: &TestSlot| {
                if slot.name == "alpha" {
                    Box::pin(stream::iter(vec![Err(PoolFailureError::from(
                        serde_json::json!({"status": 401, "message": "unauthorized"}),
                    ))]))
                } else {
                    Box::pin(stream::iter(vec![Ok(1u32)]))
                }
            }),
            classify: None,
            is_committed_output: None,
            persist_block: Some(Box::new(move |slot: &TestSlot| {
                let persisted = persisted_clone.clone();
                let slot = slot.clone();
                Box::pin(async move {
                    persisted.lock().unwrap().push(slot);
                })
            })),
            on_rotate: None,
            now: Box::new(|| NOW),
            base_block_ms: DEFAULT_SLOT_BLOCK_MS,
        };
        run_slot_failover(options).await.unwrap();
        let persisted = persisted.lock().unwrap();
        assert_eq!(persisted[0].name, "alpha");
        assert_eq!(persisted[0].block_reason.as_deref(), Some("auth_error"));
        assert!(persisted[0].blocked_until.is_none());
    }

    #[tokio::test]
    async fn a_fail_class_error_throws_immediately_without_trying_siblings() {
        let attempted = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let attempted_clone = attempted.clone();
        let options = PoolFailoverOptions::<TestSlot, u32> {
            slots: vec![make_slot("alpha"), make_slot("beta")],
            select: Box::new(first_unblocked),
            run_attempt: Box::new(move |slot: &TestSlot| {
                attempted_clone.lock().unwrap().push(slot.name.clone());
                Box::pin(stream::iter(vec![Err(PoolFailureError::from("totally unknown"))]))
            }),
            classify: None,
            is_committed_output: None,
            persist_block: None,
            on_rotate: None,
            now: Box::new(|| NOW),
            base_block_ms: DEFAULT_SLOT_BLOCK_MS,
        };
        let error = run_slot_failover(options).await.unwrap_err();
        assert_eq!(error.classification.action, PoolFailureAction::Fail);
        assert_eq!(*attempted.lock().unwrap(), vec!["alpha".to_string()]);
    }

    #[tokio::test]
    async fn a_retry_class_error_throws_for_the_outer_retry_policy_without_rotation() {
        let attempted = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let attempted_clone = attempted.clone();
        let options = PoolFailoverOptions::<TestSlot, u32> {
            slots: vec![make_slot("alpha"), make_slot("beta")],
            select: Box::new(first_unblocked),
            run_attempt: Box::new(move |slot: &TestSlot| {
                attempted_clone.lock().unwrap().push(slot.name.clone());
                Box::pin(stream::iter(vec![Err(PoolFailureError::from(
                    serde_json::json!({"status": 503, "message": "service unavailable"}),
                ))]))
            }),
            classify: None,
            is_committed_output: None,
            persist_block: None,
            on_rotate: None,
            now: Box::new(|| NOW),
            base_block_ms: DEFAULT_SLOT_BLOCK_MS,
        };
        let error = run_slot_failover(options).await.unwrap_err();
        assert_eq!(*attempted.lock().unwrap(), vec!["alpha".to_string()]);
        assert_eq!(error.classification.action, PoolFailureAction::Retry);
        assert!(!error.suppress_turn_retry);
    }

    #[tokio::test]
    async fn exhausting_every_slot_rethrows_the_last_rotate_class_failure() {
        let options = PoolFailoverOptions::<TestSlot, u32> {
            slots: vec![make_slot("alpha"), make_slot("beta")],
            select: Box::new(first_unblocked),
            run_attempt: Box::new(|_slot| {
                Box::pin(stream::iter(vec![Err(PoolFailureError::from(
                    serde_json::json!({"status": 429, "message": "rate limited"}),
                ))]))
            }),
            classify: None,
            is_committed_output: None,
            persist_block: None,
            on_rotate: None,
            now: Box::new(|| NOW),
            base_block_ms: DEFAULT_SLOT_BLOCK_MS,
        };
        let error = run_slot_failover(options).await.unwrap_err();
        assert_eq!(error.classification.action, PoolFailureAction::Rotate);
    }
}
