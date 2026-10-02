use maho_ext_api::{ExtensionApi, ExtensionFailure};

pub const WAKE_SOURCE_STATE_EVENT: &str = "wake_source_state";
pub const SENPI_CODEMODE_WAKE_SOURCE: &str = "senpi-codemode";

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WakeSourceStateItem {
    pub id: String,
    pub description: String,
    pub started_at_ms: f64,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WakeSourceState {
    pub source: String,
    pub active_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<Vec<WakeSourceStateItem>>,
}

pub fn emit_wake_source_state(api: &ExtensionApi, state: &WakeSourceState) -> Result<(), ExtensionFailure> {
    let value = serde_json::to_value(state).map_err(|error| ExtensionFailure::new(error.to_string()))?;
    api.rpc_emit(WAKE_SOURCE_STATE_EVENT, &value)?;
    api.events.emit(WAKE_SOURCE_STATE_EVENT, &value);
    Ok(())
}
