//! Post-create pin validation (`in-process/pin-check.ts`).
use super::{
    child_handle::{ChildSession, RunnerFailureKind},
    runner::ChildSpec,
    runner_error::RunnerError,
};
use crate::runners::pinned_model_equivalence::*;

pub fn assert_pinned_model_honoured(
    spec: &ChildSpec,
    session: &dyn ChildSession,
) -> Result<(), RunnerError> {
    let Some(model) = &spec.model else {
        return Ok(());
    };
    let selected = spec
        .resolved_model
        .as_ref()
        .map(|model| ModelIdentity::new(&model.provider, &model.model_id))
        .or_else(|| parse_selected_model(spec.selected_model.as_deref()));
    let (Some(selected), Some(effective)) = (selected, session.model()) else {
        return Ok(());
    };
    let fallback_entry = CatalogModelIdentity::new(&selected.provider, &selected.id);
    let entry = model
        .downcast_ref::<CatalogModelIdentity>()
        .or(Some(&fallback_entry));
    let registry = spec
        .model_registry
        .as_ref()
        .and_then(|registry| registry.downcast_ref::<std::sync::Arc<dyn PriorityPairingRegistry>>())
        .map(|registry| registry.as_ref());
    let registry = session.priority_pairing_registry().or(registry);
    if started_on_pinned_model(&effective, &selected, entry, registry) {
        return Ok(());
    }
    Err(RunnerError::new(
        RunnerFailureKind::ModelUnavailable,
        format!(
            "the child session started on {}/{} instead of the pinned {}/{}; refusing the substitution",
            effective.provider, effective.id, selected.provider, selected.id
        ),
    ))
}
fn parse_selected_model(reference: Option<&str>) -> Option<ModelIdentity> {
    let (provider, id) = reference?.split_once('/')?;
    if provider.is_empty() || id.is_empty() {
        return None;
    }
    Some(ModelIdentity::new(provider, id))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canonical_reference_requires_both_parts() {
        assert_eq!(
            parse_selected_model(Some("provider/model/variant")),
            Some(ModelIdentity::new("provider", "model/variant"))
        );
        for value in [None, Some("model"), Some("/model"), Some("provider/")] {
            assert_eq!(parse_selected_model(value), None);
        }
    }
}
