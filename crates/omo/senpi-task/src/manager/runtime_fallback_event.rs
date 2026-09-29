//! Persists a host-applied runtime model fallback (`manager/runtime-fallback-event.ts`).

use crate::manager::child_handle::ManagedChildEvent;
use crate::state::{ResolvedModelRecord, ResolvedModelSource, TaskRecord};

/// The optional `load`/`replace` pair of `RuntimeFallbackStore`.
pub trait RuntimeFallbackStore {
    fn load_record(&self, _task_id: &str) -> Option<TaskRecord> {
        None
    }
    /// `false` when the store has no `replace` (the event is then ignored).
    fn supports_replace(&self) -> bool {
        false
    }
    fn replace_record(&self, _record: &TaskRecord) {}
}

pub fn apply_runtime_fallback_event(
    store: &dyn RuntimeFallbackStore,
    task_id: &str,
    event: &ManagedChildEvent,
) {
    if event.event_type != "retry_fallback_applied" || !store.supports_replace() {
        return;
    }
    let (Some(selector), Some(record)) = (event.to.as_deref(), store.load_record(task_id)) else {
        return;
    };
    let source = record
        .resolved_model
        .as_ref()
        .map_or(ResolvedModelSource::Category, |model| model.source);
    let Some(resolved_model) = parse_model_selector(selector, source) else {
        return;
    };
    let fallback_models = record
        .fallback_models
        .as_deref()
        .map(|candidates| remaining_fallbacks(candidates, &resolved_model));
    let fallback_attempts = append_fallback_attempts(
        record.fallback_attempts.as_deref(),
        record.resolved_model.as_ref(),
        &resolved_model,
    );
    store.replace_record(&TaskRecord {
        model: resolved_model.display.clone(),
        fallback_attempts: Some(fallback_attempts),
        updated_at: chrono::Utc::now()
            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
            .to_string(),
        fallback_models: fallback_models.or(record.fallback_models.clone()),
        resolved_model: Some(resolved_model),
        ..record
    });
}

fn same_model(left: &ResolvedModelRecord, right: &ResolvedModelRecord) -> bool {
    left.provider == right.provider && left.model_id == right.model_id
}

fn parse_model_selector(
    selector: &str,
    source: ResolvedModelSource,
) -> Option<ResolvedModelRecord> {
    let slash = selector
        .find('/')
        .filter(|&slash| slash > 0 && slash < selector.len() - 1)?;
    let colon = selector.rfind(':').filter(|&colon| colon > slash);
    let display = colon.map_or(selector, |colon| &selector[..colon]);
    let thinking = colon.map(|colon| selector[colon + 1..].to_string());
    Some(ResolvedModelRecord {
        provider: display[..slash].to_string(),
        model_id: display[slash + 1..].to_string(),
        display: display.to_string(),
        source,
        variant: None,
        reasoning_effort: thinking.clone(),
        reasoning: thinking,
    })
}

fn remaining_fallbacks(
    candidates: &[ResolvedModelRecord],
    selected: &ResolvedModelRecord,
) -> Vec<ResolvedModelRecord> {
    match candidates
        .iter()
        .position(|candidate| same_model(candidate, selected))
    {
        Some(index) => candidates[index + 1..].to_vec(),
        None => candidates.to_vec(),
    }
}

fn append_fallback_attempts(
    attempts: Option<&[ResolvedModelRecord]>,
    previous: Option<&ResolvedModelRecord>,
    selected: &ResolvedModelRecord,
) -> Vec<ResolvedModelRecord> {
    let mut next = attempts.unwrap_or_default().to_vec();
    for candidate in [previous, Some(selected)].into_iter().flatten() {
        if !next.iter().any(|attempt| same_model(attempt, candidate)) {
            next.push(candidate.clone());
        }
    }
    next
}
