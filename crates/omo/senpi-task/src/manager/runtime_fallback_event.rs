//! Persists a host-applied runtime model fallback (`manager/runtime-fallback-event.ts`).

use crate::manager::child_handle::ManagedChildEvent;
use crate::state::{
    EffectiveModelRecord, EffectiveModelSource, ResolvedModelRecord, ResolvedModelSource,
    TaskRecord,
};

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
    if !store.supports_replace() {
        return;
    }
    if event.event_type == "retry_fallback_reverted" {
        let Some(mut record) = store.load_record(task_id) else {
            return;
        };
        let restored = event
            .to
            .as_deref()
            .and_then(|s| {
                parse_model_selector(
                    s,
                    record
                        .resolved_model
                        .as_ref()
                        .map_or(ResolvedModelSource::Category, |m| m.source),
                )
            })
            .or_else(|| record.fallback_attempts.as_ref()?.first().cloned());
        let Some(restored) = restored else {
            return;
        };
        without_stale_parent_fallback(&mut record, &restored.provider, &restored.model_id);
        record.model = restored.display.clone();
        record.effective_model = Some(to_effective_record(
            &restored,
            EffectiveModelSource::FallbackRevert,
        ));
        record.resolved_model = Some(restored);
        store.replace_record(&record);
        return;
    }
    if event.event_type == "model_changed" {
        if !matches!(
            event.source.as_deref(),
            Some("set" | "cycle" | "restore" | "fallback" | "fallback-revert")
        ) {
            return;
        }
        let Some(model) = event.model.as_ref() else {
            return;
        };
        let (Some(provider), Some(id)) = (
            model.get("provider").and_then(serde_json::Value::as_str),
            model.get("id").and_then(serde_json::Value::as_str),
        ) else {
            return;
        };
        if let Some(mut record) = store.load_record(task_id) {
            let source = match event.source.as_deref() {
                Some("set") => EffectiveModelSource::Set,
                Some("cycle") => EffectiveModelSource::Cycle,
                Some("restore") => EffectiveModelSource::Restore,
                Some("fallback-revert") => EffectiveModelSource::FallbackRevert,
                _ => EffectiveModelSource::Fallback,
            };
            let mut effective = to_effective_record(
                &ResolvedModelRecord::new(ResolvedModelSource::Category, provider, id),
                source,
            );
            effective.reasoning = event.thinking_level.clone();
            effective.switched_by_operator = matches!(
                source,
                EffectiveModelSource::Set | EffectiveModelSource::Cycle
            )
            .then_some(true);
            if let Some(previous) = &record.effective_model
                && previous.provider == provider && previous.model_id == id {
                    effective.service_tier = previous.service_tier.clone();
                }
            record.effective_model = Some(effective);
            without_stale_parent_fallback(&mut record, provider, id);
            store.replace_record(&record);
        }
        return;
    }
    if event.event_type != "retry_fallback_applied" {
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
        effective_model: Some(to_effective_record(
            &resolved_model,
            EffectiveModelSource::Fallback,
        )),
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

fn to_effective_record(
    model: &ResolvedModelRecord,
    source: EffectiveModelSource,
) -> EffectiveModelRecord {
    EffectiveModelRecord {
        provider: model.provider.clone(),
        model_id: model.model_id.clone(),
        display: model.display.clone(),
        source,
        switched_by_operator: None,
        variant: model.variant.clone(),
        reasoning_effort: model.reasoning_effort.clone(),
        reasoning: model.reasoning.clone(),
        service_tier: model.service_tier.clone(),
    }
}

fn without_stale_parent_fallback(record: &mut TaskRecord, provider: &str, model_id: &str) {
    if record.parent_model.as_deref() != Some(format!("{provider}/{model_id}").as_str()) {
        record.parent_model_fallback = None;
    }
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
    let colon = selector.rfind(':').filter(|&colon| {
        colon > slash
            && matches!(
                &selector[colon + 1..],
                "off" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max"
            )
    });
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
        service_tier: None,
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

#[cfg(test)]
mod parity_tests {
    use super::*;
    #[test]
    fn selectors_keep_colons_inside_model_ids() {
        let plain = parse_model_selector("bedrock/model-v2:0", ResolvedModelSource::Category)
            .expect("selector");
        assert_eq!(plain.model_id, "model-v2:0");
        let decorated =
            parse_model_selector("bedrock/model-v2:0:high", ResolvedModelSource::Category)
                .expect("selector");
        assert_eq!(decorated.model_id, "model-v2:0");
        assert_eq!(decorated.reasoning.as_deref(), Some("high"));
    }
}
