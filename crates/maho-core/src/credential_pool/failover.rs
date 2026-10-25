//! Port of senpi packages/coding-agent/src/core/credential-pool/failover.ts.
#![allow(async_fn_in_trait)]

use crate::credential_pool::classify::{
    ClassifyContext, CredentialAction, CredentialBlock, classify_credential_failure,
};
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
    if matches!(
        slot.block_reason.as_deref(),
        Some("auth_error") | Some("account_disabled")
    ) {
        return false;
    }
    if let Some(blocked_until) = slot.blocked_until
        && blocked_until > now
    {
        return false;
    }
    true
}

pub fn no_slots_message(slots: &[RunSlot], now: u64) -> String {
    let blocked: Vec<String> = slots.iter().map(|slot| {
        let reason = slot.block_reason.as_deref().unwrap_or("blocked");
        if let Some(until) = slot.blocked_until.filter(|until| *until > now)
            && let Some(date) = i64::try_from(until).ok().and_then(chrono::DateTime::from_timestamp_millis) {
                return format!("{} ({reason} until {})", slot.name, date.to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
            }
        if reason == "account_disabled" {
            format!("{} (account_disabled until re-login)", slot.name)
        } else {
            format!("{} ({reason})", slot.name)
        }
    }).collect();
    if blocked.is_empty() { "No credential slots available".to_owned() }
    else { format!("No credential slots available: {}", blocked.join(", ")) }
}

pub fn soonest_retry_at(slots: &[RunSlot], now: u64) -> Option<u64> {
    slots
        .iter()
        .filter_map(|slot| slot.blocked_until)
        .filter(|until| *until > now)
        .min()
}

pub trait FailoverHost {
    type Event: Clone;
    /// One running attempt, settled explicitly before a replacement starts.
    type Attempt;

    /// Re-read before every distinct-credential attempt so a newly added slot participates.
    async fn list_slots(&mut self) -> Vec<RunSlot>;
    fn select(&self, candidates: &[RunSlot]) -> Option<RunSlot>;
    /// Starts one attempt; an `Err` is the attempt failing by throwing before any event.
    async fn run_attempt(&mut self, slot: &RunSlot) -> Result<Self::Attempt, Value>;
    /// The attempt's next event: `None` once it ended, `Err` once it failed by throwing.
    async fn next_event(
        &mut self,
        attempt: &mut Self::Attempt,
    ) -> Option<Result<Self::Event, Value>>;
    async fn settle_attempt(&mut self, attempt: &mut Self::Attempt) -> Result<(), String>;
    fn is_cancelled(&self) -> bool {
        false
    }
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
        classify_credential_failure(
            error,
            &ClassifyContext {
                failure_count: Some(failure_count),
                ..ClassifyContext::default()
            },
        )
    }
    /// Persisted BEFORE a replacement slot is selected so a crash never forgets a block.
    async fn persist_block(
        &mut self,
        slot: &RunSlot,
        block: &CredentialBlock,
    ) -> Result<(), String>;
    async fn on_rotate(&mut self, _event: CredentialFailoverEvent) {}
    async fn on_success(&mut self, _slot: &RunSlot) -> Result<(), String> {
        Ok(())
    }
    fn now(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis() as u64)
            .unwrap_or(0)
    }
    fn provider_id(&self) -> Option<&str> {
        None
    }
    fn rotation_consented(&self) -> bool {
        self.provider_id()
            .is_none_or(crate::provider_consent::rotation_consented)
    }
    /// Delivers one event to the caller.
    fn emit(&mut self, event: Self::Event);
}

/// When every slot is cooling and this request has tried none, the rate-limited slot due back first
/// still gets the request: a provider can lift a limit before its predicted expiry.
fn cooling_probe(slots: &[RunSlot], attempted: &[String]) -> Vec<RunSlot> {
    if !attempted.is_empty() {
        return Vec::new();
    }
    slots
        .iter()
        .filter(|slot| {
            slot.block_reason.as_deref() == Some("rate_limit") && slot.blocked_until.is_some()
        })
        .min_by_key(|slot| slot.blocked_until.unwrap_or(u64::MAX))
        .cloned()
        .into_iter()
        .collect()
}

/// Generic in-lane credential failover. Runs at most one failover attempt per slot per request,
/// retries provider-scoped faults on the SAME slot up to the classifier's bound without blocking it,
/// and rotates only while no committed output has reached the caller.
pub async fn run_credential_failover<H: FailoverHost>(
    host: &mut H,
) -> Result<(), CredentialFailoverError> {
    let mut attempted: Vec<String> = Vec::new();
    let mut retries_by_slot: std::collections::HashMap<String, u32> =
        std::collections::HashMap::new();
    let mut last_error: Option<CredentialFailoverError> = None;
    let mut last_original: Option<Value> = None;
    let mut last_terminal_event: Option<H::Event> = None;
    let mut start_announced = false;

    loop {
        if host.is_cancelled() {
            return Ok(());
        }
        let slots = host.list_slots().await;
        if host.is_cancelled() {
            return Ok(());
        }
        let now = host.now();
        let rotation_consented = host.rotation_consented();
        let pinned_or_first = if slots.len() > 1 && !rotation_consented {
            slots
                .iter()
                .find(|slot| slot.pinned)
                .or_else(|| slots.first())
                .map(|slot| slot.name.clone())
        } else {
            None
        };
        let available: Vec<RunSlot> = slots
            .iter()
            .filter(|slot| {
                !attempted.contains(&slot.name)
                    && is_available(slot, now)
                    && pinned_or_first.as_ref().is_none_or(|p| &slot.name == p)
            })
            .cloned()
            .collect();
        let candidates: Vec<RunSlot> = if available.is_empty() {
            let probe = cooling_probe(&slots, &attempted);
            if let Some(ref p) = pinned_or_first {
                probe.into_iter().filter(|slot| &slot.name == p).collect()
            } else {
                probe
            }
        } else {
            available
        };
        if candidates.is_empty() {
            if let Some(terminal) = last_terminal_event.take() {
                host.emit(terminal);
                return Ok(());
            }
            let retry_at = soonest_retry_at(&slots, now);
            let (action, detail) = match (last_error, last_original) {
                (Some(error), _) => (error.action, error.detail),
                (None, Some(original)) => (CredentialAction::FailRequest, original.to_string()),
                (None, None) => (
                    CredentialAction::FailRequest,
                    no_slots_message(&slots, now),
                ),
            };
            return Err(CredentialFailoverError {
                action,
                detail,
                retry_at,
            });
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
        let failure = match host.run_attempt(&slot).await {
            Err(failure) => Some(failure),
            Ok(mut attempt) => {
                let failure = loop {
                    match host.next_event(&mut attempt).await {
                        None => break None,
                        Some(Err(failure)) => break Some(failure),
                        Some(Ok(event)) => {
                            if let Some(failure) = host.error_from_event(&event) {
                                terminal_event = Some(event);
                                break Some(failure);
                            }
                            if host.is_stream_start(&event) {
                                if start_announced {
                                    continue;
                                }
                                start_announced = true;
                            }
                            committed_output = committed_output || host.is_committed_output(&event);
                            host.emit(event);
                        }
                    }
                };
                host.settle_attempt(&mut attempt).await.map_err(|detail| {
                    CredentialFailoverError {
                        action: CredentialAction::FailRequest,
                        detail,
                        retry_at: None,
                    }
                })?;
                failure
            }
        };
        let Some(failure) = failure else {
            if host.is_cancelled() {
                return Ok(());
            }
            return host
                .on_success(&slot)
                .await
                .map_err(|detail| CredentialFailoverError {
                    action: CredentialAction::FailRequest,
                    detail,
                    retry_at: None,
                });
        };
        if host.is_cancelled() {
            return Ok(());
        }
        last_original = Some(failure.clone());
        last_terminal_event = terminal_event.clone();

        let failure_count =
            slot.failure_count.unwrap_or(0) + retries_by_slot.get(&slot.name).copied().unwrap_or(0);
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
            return Err(CredentialFailoverError {
                action,
                detail: failure.to_string(),
                retry_at: None,
            });
        }
        let CredentialAction::Failover { block } = action.clone() else {
            if let Some(terminal) = terminal_event {
                host.emit(terminal);
                return Ok(());
            }
            return Err(CredentialFailoverError {
                action,
                detail: failure.to_string(),
                retry_at: None,
            });
        };

        host.persist_block(&slot, &block)
            .await
            .map_err(|detail| CredentialFailoverError {
                action: CredentialAction::FailRequest,
                detail,
                retry_at: None,
            })?;
        if host.is_cancelled() {
            return Ok(());
        }
        attempted.push(slot.name.clone());

        // SAFETY-DIVERGENCE(packages/coding-agent/src/core/credential-pool/failover.ts): upstream rotates across credential slots on rate limits without consent; mhc gates automatic rotation behind providerRiskConsent.<provider>:rotation.
        let is_rate_limit = matches!(block, CredentialBlock::RateLimit { .. });
        if slots.len() > 1 && !rotation_consented {
            let error = CredentialFailoverError {
                action,
                detail: if is_rate_limit {
                    let provider = host.provider_id().unwrap_or("unknown");
                    let mut detail_value = failure.clone();
                    if let Some(msg) = detail_value.get("message").and_then(Value::as_str) {
                        let appended = crate::provider_consent::append_rotation_notice(
                            msg,
                            provider,
                            slots.len(),
                        );
                        if let Some(obj) = detail_value.as_object_mut() {
                            obj.insert("message".to_string(), Value::String(appended));
                        }
                    } else if let Some(s) = detail_value.as_str() {
                        detail_value = Value::String(crate::provider_consent::append_rotation_notice(
                            s,
                            provider,
                            slots.len(),
                        ));
                    } else {
                        detail_value = Value::String(crate::provider_consent::append_rotation_notice(
                            &detail_value.to_string(),
                            provider,
                            slots.len(),
                        ));
                    }
                    detail_value.to_string()
                } else {
                    failure.to_string()
                },
                retry_at: None,
            };
            if let Some(terminal) = terminal_event {
                host.emit(terminal);
                return Ok(());
            }
            return Err(error);
        }

        let error = CredentialFailoverError {
            action,
            detail: failure.to_string(),
            retry_at: None,
        };
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
        settlements: usize,
        attempt_settlement_counts: Vec<usize>,
        now: u64,
        provider_id: Option<String>,
        attempted_slots: Vec<String>,
        consented: Option<bool>,
    }

    impl FailoverHost for FakeHost {
        type Event = &'static str;
        type Attempt = std::collections::VecDeque<Result<&'static str, Value>>;

        async fn list_slots(&mut self) -> Vec<RunSlot> {
            self.slots.clone()
        }

        fn select(&self, candidates: &[RunSlot]) -> Option<RunSlot> {
            candidates.first().cloned()
        }

        async fn run_attempt(&mut self, slot: &RunSlot) -> Result<Self::Attempt, Value> {
            self.attempted_slots.push(slot.name.clone());
            self.attempt_settlement_counts.push(self.settlements);
            let scripted = self
                .attempts
                .get(self.attempt_index)
                .cloned()
                .unwrap_or_default();
            self.attempt_index += 1;
            let mut attempt = std::collections::VecDeque::new();
            for step in scripted {
                match step {
                    Ok(events) => attempt.extend(events.into_iter().map(Ok)),
                    Err(failure) => {
                        attempt.push_back(Err(failure));
                        break;
                    }
                }
            }
            Ok(attempt)
        }

        async fn next_event(
            &mut self,
            attempt: &mut Self::Attempt,
        ) -> Option<Result<Self::Event, Value>> {
            attempt.pop_front()
        }

        async fn settle_attempt(&mut self, _attempt: &mut Self::Attempt) -> Result<(), String> {
            self.settlements += 1;
            Ok(())
        }

        fn is_committed_output(&self, event: &Self::Event) -> bool {
            *event == "delta"
        }

        async fn persist_block(
            &mut self,
            slot: &RunSlot,
            block: &CredentialBlock,
        ) -> Result<(), String> {
            self.blocked.push((slot.name.clone(), block.clone()));
            Ok(())
        }

        async fn on_rotate(&mut self, _event: CredentialFailoverEvent) {
            self.rotations += 1;
        }

        async fn on_success(&mut self, slot: &RunSlot) -> Result<(), String> {
            self.successes.push(slot.name.clone());
            Ok(())
        }

        fn now(&self) -> u64 {
            self.now
        }

        fn provider_id(&self) -> Option<&str> {
            self.provider_id.as_deref()
        }

        fn rotation_consented(&self) -> bool {
            if let Some(c) = self.consented {
                return c;
            }
            self.provider_id
                .as_deref()
                .is_none_or(crate::provider_consent::rotation_consented)
        }

        fn emit(&mut self, event: Self::Event) {
            self.emitted.push(event);
        }
    }

    fn slot(name: &str) -> RunSlot {
        RunSlot {
            name: name.to_owned(),
            ..RunSlot::default()
        }
    }

    fn ok(events: Vec<&'static str>) -> Result<Vec<&'static str>, Value> {
        Ok(events)
    }

    fn failed(error: Value) -> Result<Vec<&'static str>, Value> {
        Err(error)
    }

    #[tokio::test]
    async fn replacement_starts_after_attempt_settlement() {
        let mut host = FakeHost {
            slots: vec![slot("a"), slot("b")],
            attempts: vec![
                vec![failed(json!({"status":401,"message":"invalid api key"}))],
                vec![ok(vec!["delta"])],
            ],
            ..Default::default()
        };
        run_credential_failover(&mut host).await.expect("failover");
        assert_eq!(host.attempt_settlement_counts, vec![0, 1]);
        assert_eq!(host.settlements, 2);
    }

    #[tokio::test]
    async fn rotates_to_the_next_slot_after_an_auth_failure() {
        let mut host = FakeHost {
            slots: vec![slot("a"), slot("b")],
            attempts: vec![
                vec![
                    ok(vec!["start"]),
                    failed(json!({ "status": 401, "message": "invalid api key" })),
                ],
                vec![ok(vec!["delta"])],
            ],
            ..FakeHost::default()
        };
        run_credential_failover(&mut host).await.expect("failover");
        assert_eq!(host.emitted, vec!["start", "delta"]);
        assert_eq!(
            host.blocked,
            vec![("a".to_owned(), CredentialBlock::AuthError)]
        );
        assert_eq!(host.rotations, 1);
        assert_eq!(host.successes, vec!["b".to_owned()]);
    }

    #[tokio::test]
    async fn retries_the_same_slot_for_a_provider_fault_then_succeeds() {
        let mut host = FakeHost {
            slots: vec![slot("a")],
            attempts: vec![
                vec![
                    ok(vec!["start"]),
                    failed(json!({ "status": 503, "message": "overloaded" })),
                ],
                vec![ok(vec!["delta"])],
            ],
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
            attempts: vec![vec![
                ok(vec!["start", "delta"]),
                failed(json!({ "status": 401, "message": "invalid api key" })),
            ]],
            ..FakeHost::default()
        };
        let error = run_credential_failover(&mut host).await.expect_err("error");
        assert_eq!(
            error.action,
            CredentialAction::Failover {
                block: CredentialBlock::AuthError
            }
        );
        assert_eq!(host.emitted, vec!["start", "delta"]);
        assert_eq!(host.rotations, 1);
        assert_eq!(host.blocked.len(), 1);
        assert_eq!(host.successes, Vec::<String>::new());
    }

    #[tokio::test]
    async fn a_provider_fault_after_committed_output_is_not_retried() {
        let mut host = FakeHost {
            slots: vec![slot("a")],
            attempts: vec![vec![
                ok(vec!["start", "delta"]),
                failed(json!({ "status": 503, "message": "overloaded" })),
            ]],
            ..FakeHost::default()
        };
        let error = run_credential_failover(&mut host).await.expect_err("error");
        assert_eq!(
            error.action,
            CredentialAction::RetrySame {
                max_attempts: RETRY_SAME_MAX_ATTEMPTS
            }
        );
        assert_eq!(host.emitted, vec!["start", "delta"]);
        assert_eq!(host.attempt_index, 1);
    }

    #[tokio::test]
    async fn a_blocked_slot_is_skipped_and_an_empty_pool_reports_the_soonest_retry() {
        let mut blocked = slot("a");
        blocked.blocked_until = Some(500);
        let mut host = FakeHost {
            slots: vec![blocked],
            now: 100,
            ..FakeHost::default()
        };
        let error = run_credential_failover(&mut host).await.expect_err("error");
        assert_eq!(error.retry_at, Some(500));
        assert_eq!(error.detail, "No credential slots available: a (blocked until 1970-01-01T00:00:00.500Z)");
    }

    #[tokio::test]
    async fn rotation_consent_gates_generic_failover_loop() {
        let dir = tempfile::tempdir().expect("tempdir");
        let agent_dir = dir.path().to_string_lossy().into_owned();
        let mut settings = crate::settings_manager::SettingsManager::create(&agent_dir, &agent_dir, &agent_dir, false);
        let provider = "chatgpt-subscription";

        // (1) Two accounts and no consent -> exact attempt sequence [a1] and error text with notice
        let mut host_no_consent = FakeHost {
            provider_id: Some(provider.to_owned()),
            consented: Some(false),
            slots: vec![slot("a1"), slot("a2")],
            attempts: vec![
                vec![failed(json!({ "status": 429, "message": "rate limit reached" }))],
                vec![ok(vec!["delta"])],
            ],
            ..FakeHost::default()
        };
        let error = run_credential_failover(&mut host_no_consent).await.expect_err("failover gated");
        assert_eq!(host_no_consent.attempted_slots, vec!["a1"]);
        assert!(error.detail.contains("Automatic rotation across 2 accounts is off. Enable it with `mhc auth consent chatgpt-subscription --rotation`."));

        // (2) With consent -> exact attempt sequence [a1, a2]
        crate::provider_consent::record_rotation_consent(&mut settings, provider).expect("consent");
        let mut host_with_consent = FakeHost {
            provider_id: Some(provider.to_owned()),
            consented: Some(true),
            slots: vec![slot("a1"), slot("a2")],
            attempts: vec![
                vec![failed(json!({ "status": 429, "message": "rate limit reached" }))],
                vec![ok(vec!["delta"])],
            ],
            ..FakeHost::default()
        };
        run_credential_failover(&mut host_with_consent).await.expect("failover succeeded");
        assert_eq!(host_with_consent.attempted_slots, vec!["a1", "a2"]);
        assert_eq!(host_with_consent.successes, vec!["a2"]);

        // (3) After --revoke -> exact attempt sequence [a1] and error text with notice
        crate::provider_consent::revoke_rotation_consent(&mut settings, provider).expect("revoke");
        let mut host_after_revoke = FakeHost {
            provider_id: Some(provider.to_owned()),
            consented: Some(false),
            slots: vec![slot("a1"), slot("a2")],
            attempts: vec![
                vec![failed(json!({ "status": 429, "message": "rate limit reached" }))],
                vec![ok(vec!["delta"])],
            ],
            ..FakeHost::default()
        };
        let error_revoked = run_credential_failover(&mut host_after_revoke).await.expect_err("failover gated after revoke");
        assert_eq!(host_after_revoke.attempted_slots, vec!["a1"]);
        assert!(error_revoked.detail.contains("Automatic rotation across 2 accounts is off. Enable it with `mhc auth consent chatgpt-subscription --rotation`."));
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
