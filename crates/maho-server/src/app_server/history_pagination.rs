use super::registry::JsonRpcError;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HistoryKind {
    Turn,
    Item,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SortDirection {
    Asc,
    Desc,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HistoryCursor {
    kind: HistoryKind,
    thread_id: String,
    turn_id: Option<String>,
    sort_direction: Option<SortDirection>,
    anchor: String,
    include_anchor: bool,
}
pub struct HistoryValue<T> {
    pub key: String,
    pub value: T,
}
pub struct HistoryPaginationOptions {
    pub kind: HistoryKind,
    pub thread_id: String,
    pub turn_id: Option<String>,
    pub limit: usize,
    pub sort_direction: SortDirection,
    pub cursor: Option<String>,
}
#[derive(Debug, PartialEq, Eq)]
pub struct HistoryPage<T> {
    pub data: Vec<T>,
    pub next_cursor: Option<String>,
    pub backwards_cursor: Option<String>,
}
fn invalid(message: impl Into<String>) -> JsonRpcError {
    JsonRpcError::new(-32600, message)
}
fn encode(cursor: &HistoryCursor) -> Result<String, JsonRpcError> {
    serde_json::to_string(cursor).map_err(|error| JsonRpcError::new(-32603, error.to_string()))
}
pub fn inclusive_turn_history_cursor(
    thread_id: String,
    turn_id: String,
) -> Result<String, JsonRpcError> {
    encode(&HistoryCursor {
        kind: HistoryKind::Turn,
        thread_id,
        turn_id: None,
        sort_direction: None,
        anchor: turn_id,
        include_anchor: true,
    })
}
pub fn paginate_history<T: Clone>(
    values: &[HistoryValue<T>],
    options: &HistoryPaginationOptions,
) -> Result<HistoryPage<T>, JsonRpcError> {
    let cursor = if let Some(text) = &options.cursor {
        let raw: serde_json::Value =
            serde_json::from_str(text).map_err(|_| invalid(format!("invalid cursor: {text}")))?;
        if !raw.is_object() || raw.get("turnId").is_none() || raw.get("sortDirection").is_none() {
            return Err(invalid(format!("invalid cursor: {text}")));
        }
        let parsed: HistoryCursor =
            serde_json::from_value(raw).map_err(|_| invalid(format!("invalid cursor: {text}")))?;
        if parsed.kind != options.kind
            || parsed.thread_id != options.thread_id
            || parsed.turn_id != options.turn_id
        {
            return Err(invalid(
                "invalid cursor: cursor is not scoped to this history request",
            ));
        }
        if let Some(direction) = parsed.sort_direction {
            let matches = direction == options.sort_direction;
            if if parsed.include_anchor {
                matches
            } else {
                !matches
            } {
                return Err(invalid(
                    "invalid cursor: cursor direction does not match the request",
                ));
            }
        }
        Some(parsed)
    } else {
        None
    };
    let mut ordered: Vec<_> = values.iter().collect();
    if options.sort_direction == SortDirection::Desc {
        ordered.reverse();
    }
    let start = if let Some(cursor) = cursor {
        let index = ordered
            .iter()
            .position(|value| value.key == cursor.anchor)
            .ok_or_else(|| invalid("invalid cursor: anchor is no longer present"))?;
        index + usize::from(!cursor.include_anchor)
    } else {
        0
    };
    let window = &ordered[start..];
    let page = &window[..options.limit.min(window.len())];
    let cursor_for = |anchor: String, include_anchor| {
        encode(&HistoryCursor {
            kind: options.kind,
            thread_id: options.thread_id.clone(),
            turn_id: options.turn_id.clone(),
            sort_direction: Some(options.sort_direction),
            anchor,
            include_anchor,
        })
    };
    let next_cursor = if window.len() > page.len() {
        Some(cursor_for(
            page.last()
                .map(|value| value.key.clone())
                .unwrap_or_default(),
            false,
        )?)
    } else {
        None
    };
    let backwards_cursor = page
        .first()
        .map(|value| cursor_for(value.key.clone(), true))
        .transpose()?;
    Ok(HistoryPage {
        data: page.iter().map(|value| value.value.clone()).collect(),
        next_cursor,
        backwards_cursor,
    })
}
