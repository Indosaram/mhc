//! Port of senpi packages/ai/src/api/cursor-agent/reasoning-params.ts.
// ported by todo 12

use crate::cursor::selection_descriptor::resolve_cursor_selection_descriptor;
use crate::model::Model;
use crate::types::ThinkingSelection;

use super::r#gen::agent_pb::{RequestedModel, RequestedModelModelParameterbytes};

pub struct RequestedModelFields {
    pub model_id: String,
    pub max_mode: bool,
    pub parameters: Vec<RequestedModelParameter>,
}

pub struct RequestedModelParameter {
    pub id: String,
    pub value: String,
}

/// Render the resolved Cursor selection into the protobuf `RequestedModel`
/// fields: resolved wire id, maxMode, and ordered parameters. An absent
/// selection yields the pre-grouping request shape (upstream id, no
/// parameters) byte-for-byte.
pub fn build_requested_model_fields(
    model: &Model,
    selection: Option<&ThinkingSelection>,
) -> RequestedModelFields {
    let resolved = resolve_cursor_selection_descriptor(model, selection);
    let max_mode = model
        .compat
        .as_ref()
        .and_then(|compat| compat.cursor_agent().cursor_max_mode)
        .unwrap_or(false);
    RequestedModelFields {
        model_id: resolved.model_id,
        max_mode,
        parameters: resolved
            .parameters
            .into_iter()
            .map(|parameter| RequestedModelParameter { id: parameter.id.to_owned(), value: parameter.value })
            .collect(),
    }
}

pub fn build_requested_model(model: &Model, selection: Option<&ThinkingSelection>) -> RequestedModel {
    let fields = build_requested_model_fields(model, selection);
    RequestedModel {
        model_id: fields.model_id,
        max_mode: fields.max_mode,
        parameters: fields
            .parameters
            .into_iter()
            .map(|parameter| RequestedModelModelParameterbytes { id: parameter.id, value: parameter.value })
            .collect(),
        ..Default::default()
    }
}
