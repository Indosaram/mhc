//! Port of senpi packages/coding-agent/src/core/session-settled.ts.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::session_archived::session_marker_data;
use crate::session_manager::{SessionManager, now_iso_millis};

pub const SESSION_SETTLED_CUSTOM_TYPE: &str = "session_settled";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ThreadSettledBy {
    User,
    #[serde(rename_all = "camelCase")]
    Agent {
        #[serde(skip_serializing_if = "Option::is_none")]
        thread_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        thread_title: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    PrMerged {
        #[serde(skip_serializing_if = "Option::is_none")]
        pull_request_number: Option<u64>,
    },
    #[serde(rename_all = "camelCase")]
    PrClosed {
        #[serde(skip_serializing_if = "Option::is_none")]
        pull_request_number: Option<u64>,
    },
    Inactivity {
        days: u64,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSettled {
    pub settled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub by: Option<ThreadSettledBy>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettledFields {
    pub settled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settled_by: Option<ThreadSettledBy>,
}

pub const NOT_SETTLED: SessionSettled = SessionSettled {
    settled: false,
    by: None,
};

const BY_REFUSAL: &str = "set_session_settled: by must be a ThreadSettledBy (kind user, agent, pr-merged, pr-closed or inactivity, with its fields)";

fn optional_text(value: Option<&Value>) -> Result<Option<String>, ()> {
    let Some(value) = value else {
        return Ok(None);
    };
    let Some(s) = value.as_str() else {
        return Err(());
    };
    let trimmed = s.trim();
    if trimmed.is_empty() {
        Err(())
    } else {
        Ok(Some(trimmed.to_owned()))
    }
}

fn integer_at_least(value: Option<&Value>, min: u64) -> Option<u64> {
    let n = value?.as_u64()?;
    (n >= min).then_some(n)
}

pub fn parse_thread_settled_by(value: &Value) -> Option<ThreadSettledBy> {
    let by = value.as_object()?;
    let kind = by.get("kind")?.as_str()?;
    match kind {
        "user" => Some(ThreadSettledBy::User),
        "agent" => {
            let thread_id = optional_text(by.get("threadId")).ok()?;
            let thread_title = optional_text(by.get("threadTitle")).ok()?;
            Some(ThreadSettledBy::Agent {
                thread_id,
                thread_title,
            })
        }
        "pr-merged" => {
            let pr = match by.get("pullRequestNumber") {
                None => None,
                Some(v) => Some(integer_at_least(Some(v), 1)?),
            };
            Some(ThreadSettledBy::PrMerged {
                pull_request_number: pr,
            })
        }
        "pr-closed" => {
            let pr = match by.get("pullRequestNumber") {
                None => None,
                Some(v) => Some(integer_at_least(Some(v), 1)?),
            };
            Some(ThreadSettledBy::PrClosed {
                pull_request_number: pr,
            })
        }
        "inactivity" => {
            let days = integer_at_least(by.get("days"), 0)?;
            Some(ThreadSettledBy::Inactivity { days })
        }
        _ => None,
    }
}

pub fn parse_session_settled(
    settled: Option<&Value>,
    by: Option<&Value>,
) -> Result<SessionSettled, String> {
    let Some(settled) = settled.and_then(Value::as_bool) else {
        return Err("set_session_settled: settled must be a boolean".to_owned());
    };
    let Some(by_raw) = by else {
        return Ok(SessionSettled { settled, by: None });
    };
    let Some(by) = parse_thread_settled_by(by_raw) else {
        return Err(BY_REFUSAL.to_owned());
    };
    if settled {
        Ok(SessionSettled {
            settled: true,
            by: Some(by),
        })
    } else {
        Ok(NOT_SETTLED)
    }
}

pub fn settled_state_of(entry: &Value, session_id: &str) -> Option<SessionSettled> {
    let data = session_marker_data(entry, SESSION_SETTLED_CUSTOM_TYPE, session_id)?;
    if data.get("settled").and_then(Value::as_bool) != Some(true) {
        return Some(NOT_SETTLED);
    }
    let by = data.get("by").and_then(parse_thread_settled_by);
    Some(SessionSettled { settled: true, by })
}

pub fn settled_fields(state: &SessionSettled) -> SettledFields {
    if state.settled
        && let Some(by) = &state.by
    {
        SettledFields {
            settled: true,
            settled_by: Some(by.clone()),
        }
    } else {
        SettledFields {
            settled: state.settled,
            settled_by: None,
        }
    }
}

pub fn read_session_settled(manager: &SessionManager) -> SessionSettled {
    manager.get_settled()
}

pub fn set_session_settled(manager: &mut SessionManager, state: &SessionSettled) {
    let mut data = serde_json::Map::new();
    data.insert("settled".into(), Value::Bool(state.settled));
    if state.settled
        && let Some(by) = &state.by
        && let Ok(val) = serde_json::to_value(by)
    {
        data.insert("by".into(), val);
    }
    data.insert("at".into(), Value::String(now_iso_millis()));
    data.insert("sessionId".into(), json!(manager.session_id()));
    manager.append_custom(SESSION_SETTLED_CUSTOM_TYPE, Some(Value::Object(data)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_settled_by_variants_and_validates() {
        let parsed = parse_session_settled(
            Some(&json!(true)),
            Some(&json!({"kind": "agent", "threadId": " t1 ", "threadTitle": "Helper"})),
        )
        .expect("valid");
        assert!(parsed.settled);
        assert_eq!(
            parsed.by,
            Some(ThreadSettledBy::Agent {
                thread_id: Some("t1".into()),
                thread_title: Some("Helper".into())
            })
        );

        assert!(
            parse_session_settled(
                Some(&json!(true)),
                Some(&json!({"kind": "pr-merged", "pullRequestNumber": 0}))
            )
            .is_err()
        );
        // Valid `by` is dropped when `settled` is false
        let unset = parse_session_settled(Some(&json!(false)), Some(&json!({"kind": "user"})))
            .expect("valid");
        assert_eq!(unset, NOT_SETTLED);
    }
}
