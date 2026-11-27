//! Port of senpi `packages/coding-agent/src/modes/rpc/hand-selection-commands.ts`.
//!
//! `list_hands` / `select_hand` (R4): answered only by a headless engine built with a hand
//! directory.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::rpc_types::{
    RPC_ERROR_UNKNOWN_SESSION, ResponseRecordType, RpcResponse, RpcResponseResult,
};
use crate::session_registry::RpcSessionRegistryError;

pub const HAND_SELECTION_UNSUPPORTED: &str = "hand_selection_unsupported";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandRow {
    pub device: String,
    pub label: String,
    pub platform: String,
    /// Listed but with no carrier up: it can be shown, not chosen.
    pub reachable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandRefusal {
    HandNotListed,
    HandUnreachable,
    SessionBusy,
}

impl HandRefusal {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::HandNotListed => "hand_not_listed",
            Self::HandUnreachable => "hand_unreachable",
            Self::SessionBusy => "session_busy",
        }
    }
}

pub enum SelectHandOutcome {
    Ok,
    Err(HandRefusal),
}

/// Adapter for the session hand directory / selection state (`modes/headless/hand-selection.ts`).
pub trait HandSelection: Send + Sync {
    fn hands(&self) -> Vec<HandRow>;
    fn selected(&self, durable_session_id: &str) -> Option<String>;
    fn select(&self, durable_session_id: &str, device: Option<&str>) -> SelectHandOutcome;
}

/// The parts of the router context `list_hands` and `select_hand` read.
pub trait HandSelectionHost: Send + Sync {
    fn hand_selection(&self) -> Option<&dyn HandSelection>;
    fn peek_session_id(&self, handle: &str) -> Option<String>;
    fn session_id_for_command(
        &self,
        handle: &str,
        command: &str,
    ) -> Result<Option<String>, RpcSessionRegistryError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListHandsCommand {
    pub id: Option<String>,
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectHandCommand {
    pub id: Option<String>,
    pub session_id: String,
    pub device: Option<String>,
}

fn error_response(
    id: Option<String>,
    command: &str,
    code: &str,
    data: Option<Value>,
) -> RpcResponse {
    RpcResponse {
        id,
        record_type: ResponseRecordType::Response,
        command: command.to_owned(),
        session_id: None,
        result: RpcResponseResult::Error {
            error: code.to_owned(),
            error_code: data.as_ref().map(|_| code.to_owned()),
            error_data: data,
        },
    }
}

fn ok_response(id: Option<String>, command: &str, data: Value) -> RpcResponse {
    RpcResponse {
        id,
        record_type: ResponseRecordType::Response,
        command: command.to_owned(),
        session_id: None,
        result: RpcResponseResult::Success { data: Some(data) },
    }
}

pub fn answer_list_hands(host: &dyn HandSelectionHost, command: ListHandsCommand) -> RpcResponse {
    let Some(selection) = host.hand_selection() else {
        return error_response(command.id, "list_hands", HAND_SELECTION_UNSUPPORTED, None);
    };
    let hands: Vec<Value> = selection
        .hands()
        .into_iter()
        .map(|h| {
            json!({
                "device": h.device,
                "label": h.label,
                "platform": h.platform,
                "reachable": h.reachable,
            })
        })
        .collect();
    let Some(handle) = command.session_id else {
        return ok_response(command.id, "list_hands", json!({ "hands": hands }));
    };
    let Some(durable_id) = host.peek_session_id(&handle) else {
        return error_response(command.id, "list_hands", RPC_ERROR_UNKNOWN_SESSION, None);
    };
    let selected = selection.selected(&durable_id);
    ok_response(
        command.id,
        "list_hands",
        json!({ "hands": hands, "selected": selected }),
    )
}

pub fn answer_select_hand(host: &dyn HandSelectionHost, command: SelectHandCommand) -> RpcResponse {
    let Some(selection) = host.hand_selection() else {
        return error_response(command.id, "select_hand", HAND_SELECTION_UNSUPPORTED, None);
    };
    let session_id = match host.session_id_for_command(&command.session_id, "select_hand") {
        Ok(Some(id)) => id,
        Ok(None) => {
            return error_response(command.id, "select_hand", RPC_ERROR_UNKNOWN_SESSION, None);
        }
        Err(cause) => {
            let code = if cause.code == "open_failed" {
                cause.to_string()
            } else {
                cause.code.clone()
            };
            return error_response(command.id, "select_hand", &code, cause.detail);
        }
    };
    match selection.select(&session_id, command.device.as_deref()) {
        SelectHandOutcome::Ok => ok_response(
            command.id,
            "select_hand",
            json!({
                "sessionId": command.session_id,
                "selected": command.device,
            }),
        ),
        SelectHandOutcome::Err(refusal) => error_response(
            command.id,
            "select_hand",
            refusal.as_str(),
            Some(json!({ "device": command.device })),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    struct FakeSelection {
        rows: Vec<HandRow>,
        chosen: Mutex<HashMap<String, Option<String>>>,
        busy: bool,
    }

    impl HandSelection for FakeSelection {
        fn hands(&self) -> Vec<HandRow> {
            self.rows.clone()
        }
        fn selected(&self, durable_session_id: &str) -> Option<String> {
            self.chosen
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(durable_session_id)
                .cloned()
                .flatten()
        }
        fn select(&self, durable_session_id: &str, device: Option<&str>) -> SelectHandOutcome {
            if self.busy {
                return SelectHandOutcome::Err(HandRefusal::SessionBusy);
            }
            if let Some(dev) = device {
                let Some(row) = self.rows.iter().find(|r| r.device == dev) else {
                    return SelectHandOutcome::Err(HandRefusal::HandNotListed);
                };
                if !row.reachable {
                    return SelectHandOutcome::Err(HandRefusal::HandUnreachable);
                }
            }
            self.chosen
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(durable_session_id.to_owned(), device.map(str::to_owned));
            SelectHandOutcome::Ok
        }
    }

    struct FakeHost {
        selection: Option<FakeSelection>,
        closing: bool,
    }

    impl HandSelectionHost for FakeHost {
        fn hand_selection(&self) -> Option<&dyn HandSelection> {
            self.selection.as_ref().map(|s| s as &dyn HandSelection)
        }
        fn peek_session_id(&self, handle: &str) -> Option<String> {
            (handle == "s1").then(|| "durable-1".to_owned())
        }
        fn session_id_for_command(
            &self,
            handle: &str,
            _command: &str,
        ) -> Result<Option<String>, RpcSessionRegistryError> {
            if self.closing {
                return Err(RpcSessionRegistryError::new("session_closing", None, None));
            }
            Ok(self.peek_session_id(handle))
        }
    }

    #[test]
    fn answers_unsupported_without_selection_directory() {
        let host = FakeHost {
            selection: None,
            closing: false,
        };
        let resp = answer_list_hands(
            &host,
            ListHandsCommand {
                id: Some("1".into()),
                session_id: None,
            },
        );
        assert!(matches!(
            resp.result,
            RpcResponseResult::Error { ref error, .. } if error == HAND_SELECTION_UNSUPPORTED
        ));
    }

    #[test]
    fn lists_and_selects_hands_and_checks_closing_gate() {
        let host = FakeHost {
            selection: Some(FakeSelection {
                rows: vec![
                    HandRow {
                        device: "mac".into(),
                        label: "MacBook".into(),
                        platform: "darwin".into(),
                        reachable: true,
                    },
                    HandRow {
                        device: "off".into(),
                        label: "Offline".into(),
                        platform: "linux".into(),
                        reachable: false,
                    },
                ],
                chosen: Mutex::new(HashMap::new()),
                busy: false,
            }),
            closing: false,
        };

        let sel = answer_select_hand(
            &host,
            SelectHandCommand {
                id: Some("2".into()),
                session_id: "s1".into(),
                device: Some("mac".into()),
            },
        );
        assert!(matches!(sel.result, RpcResponseResult::Success { .. }));

        let list = answer_list_hands(
            &host,
            ListHandsCommand {
                id: Some("3".into()),
                session_id: Some("s1".into()),
            },
        );
        let RpcResponseResult::Success { data: Some(data) } = list.result else {
            panic!("expected success");
        };
        assert_eq!(data["selected"], "mac");

        let unreach = answer_select_hand(
            &host,
            SelectHandCommand {
                id: Some("4".into()),
                session_id: "s1".into(),
                device: Some("off".into()),
            },
        );
        assert!(matches!(
            unreach.result,
            RpcResponseResult::Error { ref error, .. } if error == "hand_unreachable"
        ));

        let closing_host = FakeHost {
            selection: host.selection,
            closing: true,
        };
        let closing = answer_select_hand(
            &closing_host,
            SelectHandCommand {
                id: Some("5".into()),
                session_id: "s1".into(),
                device: None,
            },
        );
        assert!(matches!(
            closing.result,
            RpcResponseResult::Error { ref error, .. } if error == "session_closing"
        ));
    }
}
