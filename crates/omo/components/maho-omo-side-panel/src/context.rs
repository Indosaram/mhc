use std::sync::Arc;
use serde_json::Value;
use crate::types::{PanelHostContext, PanelUi};
pub struct PanelEventContext { pub value: Value, pub ui: Option<Arc<dyn PanelUi>> }
pub fn panel_context_from(value: &PanelEventContext) -> Option<PanelHostContext> {
    let record = value.value.as_object()?;
    Some(PanelHostContext { ui: value.ui.clone()?, mode: record.get("mode").and_then(Value::as_str).map(str::to_owned), has_ui: record.get("hasUI").and_then(Value::as_bool) == Some(true) })
}
