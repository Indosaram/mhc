//! Rust port of `packages/telemetry-core/src/otlp-batch-sender.ts`.
//!
//! Sends sealed batches oldest first, one file per request, each under a renamed claim with a lease.
//! A file whose send must be retried later is skipped until the next flush, so an outage never spins.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::PoisonError;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;

use crate::otlp_http::AttemptGate;
use crate::otlp_http::OtlpFetch;
use crate::otlp_http::PostDeps;
use crate::otlp_http::PostOutcome;
use crate::otlp_http::RandomFn;
use crate::otlp_http::max_send_ms;
use crate::otlp_http::post_with_retry;
use crate::otlp_sleep::AbortSignal;
use crate::otlp_sleep::SleepFn;
use crate::otlp_spool_dir::DropReason;
use crate::otlp_spool_dir::SpoolContext;
use crate::otlp_spool_dir::claim_batch;
use crate::otlp_spool_dir::delete_file;
use crate::otlp_spool_dir::drop_file;
use crate::otlp_spool_dir::oldest_sealed_batches;
use crate::otlp_spool_dir::quarantine_file;
use crate::otlp_spool_dir::release_claim;
use crate::otlp_spool_dir::renew_claim;
use crate::otlp_spool_file::SpoolRequest;
use crate::otlp_spool_file::spool_request;
use crate::otlp_spool_names::SpoolEntry;
use crate::types::TelemetryDiagnosticEvent;
use crate::types::TelemetryError;

pub type BatchReport =
    Arc<dyn Fn(TelemetryDiagnosticEvent, Option<TelemetryError>) + Send + Sync>;
pub type BatchConsent = Arc<dyn Fn() -> bool + Send + Sync>;

pub struct BatchSenderInput {
    pub ctx: SpoolContext,
    pub endpoint: String,
    pub ingest_key: String,
    pub fetch: OtlpFetch,
    pub request_timeout_ms: u64,
    pub lease_renew_interval_ms: u64,
    pub sleep: SleepFn,
    pub random: RandomFn,
    pub consent: BatchConsent,
    pub on_accepted: Arc<dyn Fn() + Send + Sync>,
    pub report: BatchReport,
}

pub struct BatchSender {
    input: BatchSenderInput,
    abort: AbortSignal,
    deferred: Mutex<HashSet<String>>,
    pump_lock: tokio::sync::Mutex<()>,
    rerun: AtomicBool,
    in_flight: Mutex<Option<String>>,
    stopped: AtomicBool,
}

impl BatchSender {
    pub fn new(input: BatchSenderInput) -> Self {
        Self {
            input,
            abort: AbortSignal::new(),
            deferred: Mutex::new(HashSet::new()),
            pump_lock: tokio::sync::Mutex::new(()),
            rerun: AtomicBool::new(false),
            in_flight: Mutex::new(None),
            stopped: AtomicBool::new(false),
        }
    }

    pub fn new_cycle(&self) {
        self.deferred
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }

    fn stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    fn in_flight(&self) -> Option<String> {
        self.in_flight
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn set_in_flight(&self, value: Option<String>) {
        *self
            .in_flight
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = value;
    }

    fn is_deferred(&self, name: &str) -> bool {
        self.deferred
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(name)
    }

    fn defer(&self, name: &str) {
        self.deferred
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(name.to_string());
    }

    pub async fn pump(self: &Arc<Self>) {
        let _guard = self.pump_lock.lock().await;
        loop {
            self.rerun.store(false, Ordering::SeqCst);
            self.pass().await;
            if !self.rerun.load(Ordering::SeqCst) || self.stopped() {
                break;
            }
        }
    }

    /// Releases the in-flight claim, and waits (no longer than `grace`) for a claim rename still in
    /// progress so the pass can release that one too instead of leaving it claimed until its lease ends.
    pub async fn stop(self: &Arc<Self>, grace: Duration) {
        self.stopped.store(true, Ordering::SeqCst);
        self.abort.abort();
        if let Some(in_flight) = self.in_flight() {
            self.release(&in_flight);
        }
        let _ = tokio::time::timeout(grace, self.pump_lock.lock()).await;
    }

    fn release(&self, claimed: &str) {
        let _released = release_claim(&self.input.ctx, claimed);
    }

    async fn pass(self: &Arc<Self>) {
        let mut pending: Vec<SpoolEntry> = Vec::new();
        while !self.stopped() && (self.input.consent)() {
            if pending.is_empty() {
                // One directory listing serves a whole run of sends; the spool is re-listed only once
                // that run is used up, so a pass over n files costs O(n) directory reads.
                match oldest_sealed_batches(&self.input.ctx.dir) {
                    Ok(entries) => {
                        pending = entries
                            .into_iter()
                            .filter(|entry| !self.is_deferred(entry.name()))
                            .collect();
                    }
                    Err(error) => {
                        (self.input.report)(
                            TelemetryDiagnosticEvent::TelemetrySpoolSweepFailed,
                            Some(TelemetryError::new(error.to_string())),
                        );
                        return;
                    }
                }
            }
            let Some(next) = pending.first().cloned() else {
                return;
            };
            pending.remove(0);
            self.defer(next.name());
            let SpoolEntry::Batch {
                signal, batch_id, ..
            } = next
            else {
                return;
            };
            let claimed = match claim_batch(&self.input.ctx, signal, &batch_id).await {
                Ok(claimed) => claimed,
                Err(error) => {
                    // The batch keeps its name (or its lease runs out) and a later pass retries it; the
                    // pump never rejects.
                    (self.input.report)(
                        TelemetryDiagnosticEvent::TelemetryOtlpSendFailed,
                        Some(TelemetryError::new(error.to_string())),
                    );
                    None
                }
            };
            if let Some(claimed) = claimed {
                // A claim that completes after stop() is released instead of left claimed.
                if self.stopped() {
                    self.release(&claimed);
                    return;
                }
                self.send(&claimed).await;
            }
        }
    }

    /// Re-asserts the claim by renaming it to a fresh lease right before each attempt. Rename is the
    /// mutual exclusion: if another process took the batch, the rename fails and nothing is sent.
    fn gate(&self) -> AttemptGate {
        if !(self.input.consent)() {
            return AttemptGate::Withdrawn;
        }
        let Some(current) = self.in_flight() else {
            return AttemptGate::Lost;
        };
        match renew_claim(&self.input.ctx, &current) {
            Ok(Some(renewed)) => {
                self.set_in_flight(Some(renewed));
                AttemptGate::Send
            }
            Ok(None) => AttemptGate::Lost,
            Err(_) => AttemptGate::Lost,
        }
    }

    fn spawn_lease_renewal(self: &Arc<Self>, deadline_ms: u64) -> Option<tokio::task::JoinHandle<()>> {
        let handle = tokio::runtime::Handle::try_current().ok()?;
        let sender = Arc::clone(self);
        let interval = self.input.lease_renew_interval_ms.max(1);
        Some(handle.spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_millis(interval));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            ticker.tick().await;
            loop {
                ticker.tick().await;
                // Past the longest possible send, the lease is left to expire so another process can
                // take over.
                if (sender.input.ctx.now_ms)() > deadline_ms {
                    continue;
                }
                let Some(current) = sender.in_flight() else {
                    continue;
                };
                match renew_claim(&sender.input.ctx, &current) {
                    Ok(Some(renewed)) => sender.set_in_flight(Some(renewed)),
                    Ok(None) => {}
                    Err(error) => (sender.input.report)(
                        TelemetryDiagnosticEvent::TelemetrySpoolWriteFailed,
                        Some(TelemetryError::new(error.to_string())),
                    ),
                }
            }
        }))
    }

    async fn send(self: &Arc<Self>, claimed: &str) {
        self.set_in_flight(Some(claimed.to_string()));
        match self.send_claimed(claimed).await {
            Ok(()) => {}
            Err(error) => {
                // After stop() the claim has already been released, so a read that lost the file is
                // expected.
                if !self.stopped() {
                    (self.input.report)(TelemetryDiagnosticEvent::TelemetryOtlpSendFailed, Some(error));
                    if let Some(current) = self.in_flight() {
                        self.release(&current);
                    }
                }
            }
        }
        self.set_in_flight(None);
    }

    async fn send_claimed(self: &Arc<Self>, claimed: &str) -> Result<(), TelemetryError> {
        let ctx = self.input.ctx.clone();
        let deadline_ms = (ctx.now_ms)() + max_send_ms(self.input.request_timeout_ms);
        let renewal = self.spawn_lease_renewal(deadline_ms);
        let result = self.post(claimed, &ctx).await;
        if let Some(renewal) = renewal {
            renewal.abort();
        }
        result
    }

    async fn post(self: &Arc<Self>, claimed: &str, ctx: &SpoolContext) -> Result<(), TelemetryError> {
        let bytes = std::fs::read(ctx.dir.join(claimed))
            .map_err(|error| TelemetryError::new(error.to_string()))?;
        let request = spool_request(&bytes);
        let (signal, body) = match request {
            SpoolRequest::Corrupt { .. } => {
                let _quarantined = quarantine_file(ctx, claimed);
                return Ok(());
            }
            SpoolRequest::Ok { signal, body, .. } => (signal, body),
        };
        let sender = Arc::clone(self);
        let deps = PostDeps {
            fetch: Arc::clone(&self.input.fetch),
            sleep: Arc::clone(&self.input.sleep),
            random: Arc::clone(&self.input.random),
            endpoint: self.input.endpoint.clone(),
            ingest_key: self.input.ingest_key.clone(),
            abort: self.abort.clone(),
            request_timeout_ms: self.input.request_timeout_ms,
            before_attempt: Some(Arc::new(move || sender.gate())),
        };
        let outcome = post_with_retry(&deps, signal, &body).await;
        let current = self.in_flight().unwrap_or_else(|| claimed.to_string());
        match outcome {
            PostOutcome::Accepted { partial } => {
                let _deleted = delete_file(&ctx.dir, &current);
                (self.input.on_accepted)();
                if let Some(partial) = partial
                    && partial.rejected > 0 {
                        (self.input.report)(
                            TelemetryDiagnosticEvent::TelemetryOtlpPartialSuccess,
                            Some(TelemetryError::new(partial.message)),
                        );
                    }
            }
            PostOutcome::Rejected { .. } => {
                let _dropped = drop_file(ctx, &current, DropReason::Rejected);
            }
            PostOutcome::Withdrawn => {
                let _deleted = delete_file(&ctx.dir, &current);
            }
            PostOutcome::Lost => return Ok(()),
            PostOutcome::RetryLater => {
                if !self.stopped() {
                    let _released = release_claim(ctx, &current);
                    (self.input.report)(TelemetryDiagnosticEvent::TelemetryOtlpSendFailed, None);
                }
            }
            PostOutcome::Aborted => {
                if !self.stopped() {
                    let _released = release_claim(ctx, &current);
                }
            }
        }
        Ok(())
    }
}
