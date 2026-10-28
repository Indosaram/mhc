//! `session_list` / `session_send` (spec §6) as plain functions over the store.

use crate::store::{Delivery, Endpoint, Mode, Sender, Store};
use serde_json::{Value, json};

pub const LIST_TOOL: &str = "session_list";
pub const SEND_TOOL: &str = "session_send";
pub const TOOL_NAMES: &[&str] = &[LIST_TOOL, SEND_TOOL];

/// A tool's text, details and error flag.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolOutcome {
    pub text: String,
    pub details: Value,
    pub is_error: bool,
}

fn error(text: String) -> ToolOutcome {
    ToolOutcome {
        details: json!({"error": text}),
        text,
        is_error: true,
    }
}

fn endpoint_line(endpoint: &Endpoint) -> String {
    format!(
        "- {}{} · cwd {} · since {}",
        endpoint.session_id,
        endpoint
            .name
            .as_deref()
            .map(|name| format!(" (\"{name}\")"))
            .unwrap_or_default(),
        endpoint.cwd,
        endpoint.started_at
    )
}

/// `session_list`: live sessions other than `caller`.
pub fn list(store: &Store, caller: &str) -> ToolOutcome {
    let others: Vec<Endpoint> = store
        .live()
        .into_iter()
        .filter(|endpoint| endpoint.session_id != caller)
        .collect();
    let text = if others.is_empty() {
        "No other live sessions.".to_owned()
    } else {
        let mut lines = vec![format!("Live sessions ({}):", others.len())];
        lines.extend(others.iter().map(endpoint_line));
        lines.join("\n")
    };
    ToolOutcome {
        text,
        details: json!({"sessions": others}),
        is_error: false,
    }
}

/// Who calls `session_send` and how deep in a relay chain it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Caller {
    pub session_id: String,
    pub name: Option<String>,
    /// Hop of the delivery this session last received (0 after user input).
    pub hop: u32,
}

/// `session_send {to, message, mode?}`.
pub fn send(store: &Store, caller: &Caller, max_hops: u32, params: &Value) -> ToolOutcome {
    let text_of = |key: &str| {
        params
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
    };
    let Some(to) = text_of("to") else {
        return error("`to` is required: a session id, name or id prefix".into());
    };
    let Some(message) = text_of("message") else {
        return error("`message` is required".into());
    };
    let mode = match text_of("mode") {
        None => Mode::FollowUp,
        Some(mode) => match Mode::parse(mode) {
            Some(mode) => mode,
            None => return error(format!("unknown mode \"{mode}\" (use followUp or steer)")),
        },
    };
    let hop = caller.hop.saturating_add(1);
    if hop > max_hops {
        return error(format!(
            "relay limit reached: this message would be hop {hop}, the limit is {max_hops}; answer the user instead of relaying further"
        ));
    }
    let target = match store.resolve(to, Some(&caller.session_id)) {
        Ok(target) => target,
        Err(err) => return error(err.to_string()),
    };
    let delivery = Delivery::new(
        message,
        Sender::Agent {
            session_id: caller.session_id.clone(),
            name: caller.name.clone(),
        },
        mode,
        hop,
    );
    let shown = target.name.as_deref().map_or_else(
        || target.session_id.clone(),
        |name| format!("{name} [{}]", target.session_id),
    );
    match store.deliver(&target.session_id, &delivery) {
        Ok(_) => ToolOutcome {
            text: format!(
                "Sent to session {shown} as {} (delivery {}, hop {hop}).",
                mode.as_str(),
                delivery.delivery_id
            ),
            details: json!({
                "delivery_id": delivery.delivery_id,
                "to": target.session_id,
                "mode": mode.as_str(),
                "hop": hop,
            }),
            is_error: false,
        },
        Err(err) => error(format!("could not write the delivery: {err}")),
    }
}
