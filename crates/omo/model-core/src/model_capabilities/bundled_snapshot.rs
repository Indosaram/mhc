use super::supplemental_entries::SUPPLEMENTAL_MODEL_CAPABILITIES;
use super::types::ModelCapabilitiesSnapshot;
use super::types::ModelCapabilitiesSnapshotEntry;
use super::types::SnapshotLimit;
use super::types::SnapshotModalities;
use crate::fable_55_launch::FABLE_55_LAUNCH;

// The launch placeholder only fills gaps: every field the generated catalog defines wins, so its
// undefined launch values can never hide real data once the catalog lists the model. Every other
// supplemental row keeps overriding the generated row on purpose (e.g. the grok output caps).
fn fill_gaps(
    generated: &ModelCapabilitiesSnapshotEntry,
    placeholder: &ModelCapabilitiesSnapshotEntry,
) -> ModelCapabilitiesSnapshotEntry {
    let modalities = match (&generated.modalities, &placeholder.modalities) {
        (Some(value), Some(base)) => Some(SnapshotModalities {
            input: value.input.clone().or_else(|| base.input.clone()),
            output: value.output.clone().or_else(|| base.output.clone()),
        }),
        (value, base) => value.clone().or_else(|| base.clone()),
    };
    let limit = match (&generated.limit, &placeholder.limit) {
        (Some(value), Some(base)) => Some(SnapshotLimit {
            context: value.context.or(base.context),
            input: value.input.or(base.input),
            output: value.output.or(base.output),
        }),
        (value, base) => value.clone().or_else(|| base.clone()),
    };
    ModelCapabilitiesSnapshotEntry {
        id: generated.id.clone(),
        family: generated
            .family
            .clone()
            .or_else(|| placeholder.family.clone()),
        reasoning: generated.reasoning.or(placeholder.reasoning),
        temperature: generated.temperature.or(placeholder.temperature),
        tool_call: generated.tool_call.or(placeholder.tool_call),
        modalities,
        limit,
    }
}

/// The generated models.dev snapshot with supplemental entries layered on top.
#[must_use]
pub fn get_bundled_model_capabilities_snapshot(
    snapshot_json: &ModelCapabilitiesSnapshot,
) -> ModelCapabilitiesSnapshot {
    let mut snapshot = snapshot_json.clone();
    for (model_id, entry) in SUPPLEMENTAL_MODEL_CAPABILITIES.iter() {
        snapshot.models.insert(model_id.clone(), entry.clone());
    }
    if let Some(generated_launch_row) = snapshot_json.models.get(&FABLE_55_LAUNCH.id) {
        snapshot.models.insert(
            FABLE_55_LAUNCH.id.clone(),
            fill_gaps(generated_launch_row, &FABLE_55_LAUNCH),
        );
    }
    snapshot
}
