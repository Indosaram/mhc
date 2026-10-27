use maho_ai::utils::abort::AbortController;
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug)]
pub struct JobSettlement<R, E> {
    pub result: Option<R>,
    pub error: Option<E>,
}

#[derive(Clone)]
pub struct SpeculativeJob<S, R, E> {
    pub generation: u64,
    pub snapshot: S,
    pub controller: AbortController,
    pub armed_at_tokens: u64,
    settlement: Arc<Mutex<Option<JobSettlement<R, E>>>>,
    completion: tokio::sync::watch::Receiver<bool>,
}

#[derive(Clone, Debug)]
pub struct LiveSummaryFailure {
    pub message: String,
    pub classification: crate::transient_failure::SummarizationFailure,
    /// How `classify_required_compaction_fallback_failure` sees this failure.
    pub fallback: crate::deterministic_fallback::SummaryFailure,
    /// Set for an unsafe retained suffix, so a reused failure can still latch the rejection.
    pub unsafe_diagnostics: Option<crate::deterministic_fallback::DeterministicFallbackDiagnostic>,
}

impl From<crate::speculative::SummaryGenerationError> for LiveSummaryFailure {
    fn from(error: crate::speculative::SummaryGenerationError) -> Self {
        use crate::{
            speculative::SummaryGenerationError, speculative_summary::SummaryStreamError,
            transient_failure::SummarizationFailure,
        };
        let classification = match &error {
            SummaryGenerationError::Stream(SummaryStreamError::DurationBudget) => {
                SummarizationFailure::StreamDurationBudget
            }
            SummaryGenerationError::Stream(SummaryStreamError::IdleTimeout) => {
                SummarizationFailure::StreamIdleTimeout
            }
            SummaryGenerationError::TotalBudget => SummarizationFailure::TotalBudget,
            SummaryGenerationError::Overflow(_) => SummarizationFailure::OverflowExhausted,
            SummaryGenerationError::Request(response) => {
                let transient = matches!(
                    crate::speculative::summary_request_failure(response),
                    crate::deterministic_fallback::SummaryFailure::Request {
                        transient: true,
                        ..
                    }
                );
                SummarizationFailure::SummaryRequest { transient }
            }
            SummaryGenerationError::Auth(_)
            | SummaryGenerationError::EmptySummary(_)
            | SummaryGenerationError::UnsafeRetainedSuffix(_)
            | SummaryGenerationError::Stream(SummaryStreamError::Provider(_)) => {
                SummarizationFailure::Other
            }
        };
        Self {
            message: error.to_string(),
            classification,
            fallback: crate::speculative::summary_failure(&error),
            unsafe_diagnostics: match &error {
                SummaryGenerationError::UnsafeRetainedSuffix(unsafe_error) => Some(unsafe_error.diagnostics.clone()),
                _ => None,
            },
        }
    }
}

pub type LiveSpeculativeJob = SpeculativeJob<
    crate::speculative::SpeculativeCompactionSnapshot,
    maho_ext_api::CompactionResult,
    LiveSummaryFailure,
>;

pub fn start_live_speculative_job(
    api: &maho_ext_api::ExtensionApi,
    context: &maho_ext_api::ExtensionContext,
    generation: u64,
    instructions: String,
) -> Result<Option<LiveSpeculativeJob>, maho_ext_api::ExtensionFailure> {
    // senpi startSpeculativeCompaction: a latched unsafe-retained rejection never retries automatically.
    if crate::extension_wiring::automatic_compaction_blocked(context)? {
        return Ok(None);
    }
    let Some(snapshot) = crate::speculative::create_speculative_compaction_snapshot(
        context,
        generation,
        Some(instructions),
        crate::extension_wiring::summarization_tools(api),
    )?
    else {
        return Ok(None);
    };
    let controller = AbortController::new();
    let signal = controller.signal();
    let task_snapshot = snapshot.clone();
    let registry = Arc::clone(&context.model_registry);
    let armed = context
        .get_context_usage()?
        .and_then(|usage| usage.tokens)
        .unwrap_or(0);
    let settled = async move {
        let auth = tokio::select! {
            biased;
            () = signal.cancelled() => return JobSettlement { result: None, error: None },
            auth = registry.get_api_key_and_headers(&task_snapshot.model) => auth,
        };
        let result = match auth {
            Ok(auth) => crate::extension_wiring::generate_compaction(
                &task_snapshot,
                auth.auth.api_key,
                auth.ambient,
                auth.auth.headers,
                Some(&signal),
                None,
                &|_| {},
            )
            .await
            .map_err(LiveSummaryFailure::from),
            Err(error) => Err(LiveSummaryFailure {
                message: error.to_string(),
                classification: crate::transient_failure::SummarizationFailure::Other,
                fallback: crate::deterministic_fallback::SummaryFailure::Other,
                unsafe_diagnostics: None,
            }),
        };
        match result {
            Ok(result) => JobSettlement {
                result,
                error: None,
            },
            Err(error) => JobSettlement {
                result: None,
                error: Some(error),
            },
        }
    };
    Ok(Some(track_speculative_job(
        generation, snapshot, controller, settled, armed,
    )))
}

pub fn claim_live_warm_job(
    job: &mut Option<LiveSpeculativeJob>,
    event: &maho_ext_api::SessionBeforeCompactEvent,
    context: &maho_ext_api::ExtensionContext,
) -> Option<LiveSpeculativeJob> {
    let candidate = job.as_ref()?;
    let selected = context.model.as_ref()?;
    let model = &candidate.snapshot.model;
    if event.signal.is_aborted()
        || event.custom_instructions.is_some()
        || candidate.snapshot.origin.as_deref() != Some("speculative")
        || candidate.snapshot.preparation.first_kept_entry_id
            != event.preparation.first_kept_entry_id
        || model.api != selected.api
        || model.provider != selected.provider
        || model.id != selected.id
        || model.base_url != selected.base_url
        || model.context_window != selected.context_window
    {
        return None;
    }
    let anchor = maho_core::compaction::warm_anchor::create_warm_anchor_snapshot(
        &candidate.snapshot.preparation.first_kept_entry_id,
        &candidate.snapshot.branch_entries,
    )?;
    let branch: Vec<_> = event
        .branch_entries
        .iter()
        .map(|entry| {
            let mut value = entry.data.clone();
            value["id"] = serde_json::json!(entry.id);
            value["type"] = serde_json::json!(entry.kind);
            value
        })
        .collect();
    if !maho_core::compaction::warm_anchor::is_warm_summary_anchor_valid(&anchor, &branch) {
        return None;
    }
    job.take()
}

impl<S, R: Clone, E: Clone> SpeculativeJob<S, R, E> {
    pub async fn cancel(&self) -> JobSettlement<R, E> {
        self.controller.abort(None);
        self.settled().await
    }

    pub fn completed(&self) -> bool {
        *self.completion.borrow()
    }

    pub async fn settled(&self) -> JobSettlement<R, E> {
        let mut receiver = self.completion.clone();
        receiver
            .wait_for(|completed| *completed)
            .await
            .expect("settlement sender survives until notification");
        self.settlement
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .expect("settlement precedes completion")
            .clone()
    }
}

pub fn track_speculative_job<S, R: Send + 'static, E: Send + 'static>(
    generation: u64,
    snapshot: S,
    controller: AbortController,
    settled: impl std::future::Future<Output = JobSettlement<R, E>> + Send + 'static,
    armed_at_tokens: u64,
) -> SpeculativeJob<S, R, E> {
    let settlement = Arc::new(Mutex::new(None));
    let output = Arc::clone(&settlement);
    let (sender, completion) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        let value = settled.await;
        *output
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(value);
        sender.send_replace(true);
    });
    SpeculativeJob {
        generation,
        snapshot,
        controller,
        armed_at_tokens,
        settlement,
        completion,
    }
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;

    #[tokio::test]
    async fn cancellation_waits_for_generation_cleanup_before_settling() {
        // Given a job whose generation cleanup is explicitly held open.
        let controller = AbortController::new();
        let signal = controller.signal();
        let (started, cleanup_started) = tokio::sync::oneshot::channel();
        let (release, cleanup_release) = tokio::sync::oneshot::channel();
        let job = track_speculative_job(1, (), controller, async move {
            signal.cancelled().await;
            started.send(()).expect("cleanup observed");
            cleanup_release.await.expect("cleanup release");
            JobSettlement::<(), String> { result: None, error: None }
        }, 0);

        // When cancellation requests retirement of this generation.
        let cancelling = tokio::spawn(async move { job.cancel().await });
        tokio::time::timeout(std::time::Duration::from_secs(5), cleanup_started).await
            .expect("cleanup starts").expect("cleanup notification");

        // Then cancellation cannot complete before the generation acknowledges cleanup.
        assert!(!cancelling.is_finished());
        release.send(()).expect("release cleanup");
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), cancelling).await
            .expect("cancellation settles").expect("cancellation task");
        assert!(result.result.is_none());
        assert!(result.error.is_none());
    }
}
