use super::supplemental_entries::SUPPLEMENTAL_MODEL_CAPABILITIES;
use super::types::ModelCapabilitiesSnapshot;

/// The generated models.dev snapshot with supplemental entries layered on top.
#[must_use]
pub fn get_bundled_model_capabilities_snapshot(
    snapshot_json: &ModelCapabilitiesSnapshot,
) -> ModelCapabilitiesSnapshot {
    let mut snapshot = snapshot_json.clone();
    for (model_id, entry) in SUPPLEMENTAL_MODEL_CAPABILITIES.iter() {
        snapshot.models.insert(model_id.clone(), entry.clone());
    }
    snapshot
}
