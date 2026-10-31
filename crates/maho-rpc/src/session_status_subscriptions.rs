//! `subscribe_sessions` request parsing and the per-subscription match rule (senpi `session-status-subscriptions.ts`).
//!
//! Parsed once at the wire boundary, so the stream only ever holds a well-formed selector. Every refusal names the
//! field that was wrong and what to send instead.
use crate::session_status::SessionStatusEntryView;
use serde_json::Value;
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq)]
pub enum SessionSelector {
    All,
    Ids(BTreeSet<String>),
    ChildrenOf(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum MirrorSelection {
    None,
    All,
    Types(BTreeSet<String>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct SubscriptionOptions {
    pub selector: SessionSelector,
    pub include_workers: bool,
    pub include_context: bool,
    pub mirror: MirrorSelection,
    pub effects: bool,
    pub dialogs: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SubscriptionRefusal {
    pub reason: String,
    pub action: String,
}

const MAX_SELECTOR_IDS: usize = 1_024;
const MAX_MIRROR_TYPES: usize = 64;

fn refusal(reason: impl Into<String>, action: &str) -> SubscriptionRefusal {
    SubscriptionRefusal {
        reason: reason.into(),
        action: action.into(),
    }
}

/// `^[a-z][a-z0-9_]{0,63}$`.
fn is_record_type(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 64
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_')
}

fn parse_selector(value: Option<&Value>) -> Result<SessionSelector, SubscriptionRefusal> {
    let action = r#"send sessions: "all", { ids: [<sessionId | durableSessionId | sessionPath>] } or { childrenOf: <durableSessionId> }"#;
    let value = match value {
        None => return Ok(SessionSelector::All),
        Some(Value::String(all)) if all == "all" => return Ok(SessionSelector::All),
        Some(Value::Object(value)) => value,
        Some(_) => return Err(refusal(r#"sessions must be "all" or an object"#, action)),
    };
    match (value.get("ids"), value.get("childrenOf")) {
        (Some(ids), None) => {
            let Some(ids) = ids
                .as_array()
                .filter(|ids| !ids.is_empty() && ids.len() <= MAX_SELECTOR_IDS)
            else {
                return Err(refusal(
                    format!(
                        "sessions.ids must be a non-empty array of at most {MAX_SELECTOR_IDS} strings"
                    ),
                    action,
                ));
            };
            ids.iter()
                .map(|id| id.as_str().filter(|id| !id.is_empty()).map(str::to_owned))
                .collect::<Option<BTreeSet<_>>>()
                .map(SessionSelector::Ids)
                .ok_or_else(|| {
                    refusal(
                        "every sessions.ids entry must be a non-empty string",
                        action,
                    )
                })
        }
        (None, Some(parent)) => parent
            .as_str()
            .filter(|parent| !parent.is_empty())
            .map(|parent| SessionSelector::ChildrenOf(parent.to_owned()))
            .ok_or_else(|| {
                refusal(
                    "sessions.childrenOf must be a non-empty durable session id",
                    action,
                )
            }),
        _ => Err(refusal(
            "sessions must name exactly one of ids or childrenOf",
            action,
        )),
    }
}

fn parse_mirror(value: Option<&Value>) -> Result<MirrorSelection, SubscriptionRefusal> {
    match value {
        None => return Ok(MirrorSelection::None),
        Some(Value::String(kind)) if kind == "none" => return Ok(MirrorSelection::None),
        Some(Value::String(kind)) if kind == "all" => return Ok(MirrorSelection::All),
        _ => {}
    }
    let action =
        r#"send mirror: "none", "all" or an array of record types such as ["message_update"]"#;
    let Some(types) = value
        .and_then(Value::as_array)
        .filter(|types| !types.is_empty() && types.len() <= MAX_MIRROR_TYPES)
    else {
        return Err(refusal(
            format!(
                r#"mirror must be "none", "all" or a non-empty array of at most {MAX_MIRROR_TYPES} record types"#
            ),
            action,
        ));
    };
    types
        .iter()
        .map(|kind| {
            kind.as_str()
                .filter(|kind| is_record_type(kind))
                .map(str::to_owned)
        })
        .collect::<Option<BTreeSet<_>>>()
        .map(MirrorSelection::Types)
        .ok_or_else(|| {
            refusal(
                "every mirror entry must be a record type matching ^[a-z][a-z0-9_]{0,63}$",
                action,
            )
        })
}

fn optional_boolean(request: &Value, field: &str) -> Result<Option<bool>, SubscriptionRefusal> {
    match request.get(field) {
        None => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(SubscriptionRefusal {
            reason: format!("{field} must be a boolean"),
            action: format!("send {field}: true or false, or omit it"),
        }),
    }
}

/// senpi `parseSubscription` over the raw `subscribe_sessions` request.
pub fn parse_subscription(request: &Value) -> Result<SubscriptionOptions, SubscriptionRefusal> {
    let selector = parse_selector(request.get("sessions"))?;
    let mirror = parse_mirror(request.get("mirror"))?;
    let include_workers = optional_boolean(request, "include_workers")?;
    let include_context = optional_boolean(request, "include_context")?;
    let dialogs = optional_boolean(request, "dialogs")?;
    let effects = optional_boolean(request, "effects")?;
    // A childrenOf subscription watches machine-driven children: they open as workers, and their truth is the receipts.
    let children = matches!(selector, SessionSelector::ChildrenOf(_));
    Ok(SubscriptionOptions {
        selector,
        dialogs: dialogs.unwrap_or(false),
        include_workers: include_workers.unwrap_or(children),
        include_context: include_context.unwrap_or(false),
        mirror,
        effects: effects.unwrap_or(children),
    })
}

/// senpi `subscriptionMatches`.
pub fn subscription_matches(
    options: &SubscriptionOptions,
    session_id: &str,
    entry: &SessionStatusEntryView,
) -> bool {
    if entry.kind == "worker" && !options.include_workers {
        return false;
    }
    match &options.selector {
        SessionSelector::All => true,
        SessionSelector::Ids(ids) => {
            ids.contains(session_id)
                || entry
                    .durable_session_id
                    .as_ref()
                    .is_some_and(|id| ids.contains(id))
                || entry
                    .session_path
                    .as_ref()
                    .is_some_and(|path| ids.contains(path))
        }
        SessionSelector::ChildrenOf(parent) => {
            // Lineage reads the frozen open-time context: a patch can never move a session into or out of the view.
            let lineage = entry.open_context.as_ref().unwrap_or(&entry.context);
            lineage
                .get("parent_session_id")
                .or_else(|| lineage.get("root_session_id"))
                .and_then(Value::as_str)
                == Some(parent.as_str())
        }
    }
}

/// senpi `mirrorsRecord`.
pub fn mirrors_record(mirror: &MirrorSelection, record_type: &str) -> bool {
    match mirror {
        MirrorSelection::None => false,
        MirrorSelection::All => true,
        MirrorSelection::Types(types) => types.contains(record_type),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn dialogs_is_an_optional_boolean_and_defaults_follow_the_selector() {
        let parsed =
            parse_subscription(&json!({"type":"subscribe_sessions","mirror":"all","dialogs":true}))
                .unwrap();
        assert!(parsed.dialogs && !parsed.include_workers && !parsed.effects);
        assert_eq!(parsed.mirror, MirrorSelection::All);
        let refused = parse_subscription(&json!({"dialogs":"yes"})).unwrap_err();
        assert_eq!(refused.reason, "dialogs must be a boolean");
        assert_eq!(refused.action, "send dialogs: true or false, or omit it");
        let children = parse_subscription(&json!({"sessions":{"childrenOf":"p"}})).unwrap();
        assert!(children.include_workers && children.effects && !children.dialogs);
    }

    #[test]
    fn selectors_and_mirrors_refuse_malformed_input() {
        assert!(parse_subscription(&json!({"sessions":{"ids":[]}})).is_err());
        assert!(parse_subscription(&json!({"sessions":{"ids":["a"],"childrenOf":"p"}})).is_err());
        assert!(parse_subscription(&json!({"mirror":["Bad-Type"]})).is_err());
        let types = parse_subscription(&json!({"mirror":["message_update"]}))
            .unwrap()
            .mirror;
        assert!(mirrors_record(&types, "message_update") && !mirrors_record(&types, "agent_end"));
    }
}
