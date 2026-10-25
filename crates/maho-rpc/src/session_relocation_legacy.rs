//! One-off host requests and scheduled relocation custody (senpi session-relocation.ts).
use crate::session_relocation_channel::HostResponseOutcome;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
pub const HOST_ANSWER_TIMEOUT_MS: u64 = 60_000;
fn refused(reason: &str, detail: &str) -> Value {
    json!({"status":"refused","reason":reason,"detail":detail})
}
struct Pending {
    connection: String,
    finish: tokio::sync::oneshot::Sender<Value>,
}
#[derive(Clone)]
struct Scheduled {
    move_id: String,
    target: String,
    reason: String,
    connection: String,
    aborted: bool,
}
#[derive(Default)]
struct State {
    pending: BTreeMap<String, Pending>,
    scheduled: Option<Scheduled>,
    requesting: bool,
}
#[derive(Clone, Default)]
pub struct RpcSessionRelocation {
    state: Arc<Mutex<State>>,
}
impl RpcSessionRelocation {
    pub async fn ask(
        &self,
        fields: Value,
        connection: &str,
        send: impl FnOnce(Value),
        cancelled: Option<maho_ai::utils::abort::AbortSignal>,
    ) -> Value {
        let id = format!("hr_{}", uuid::Uuid::new_v4());
        let (tx, rx) = tokio::sync::oneshot::channel();
        if cancelled.as_ref().is_some_and(|signal| signal.aborted()) {
            return refused("cancelled", "the request was cancelled");
        }
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pending
            .insert(
                id.clone(),
                Pending {
                    connection: connection.into(),
                    finish: tx,
                },
            );
        let mut record = fields;
        record["type"] = "host_request".into();
        record["id"] = id.clone().into();
        send(record);
        let receive = async {
            match rx.await {
                Ok(value) => value,
                Err(_) => refused("session_closed", "this session was closed"),
            }
        };
        let wait = async {
            if let Some(signal) = cancelled {
                tokio::select! {answer=receive=>answer,_=signal.cancelled()=>refused("cancelled","the request was cancelled")}
            } else {
                receive.await
            }
        };
        let answer = tokio::time::timeout(
            std::time::Duration::from_millis(HOST_ANSWER_TIMEOUT_MS),
            wait,
        )
        .await
        .unwrap_or_else(|_| refused("no_answer", "the app did not answer in time"));
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pending
            .remove(&id);
        answer
    }
    pub async fn targets(&self, connection: Option<&str>, send: impl FnOnce(Value)) -> Value {
        self.targets_with_signal(connection, send, None).await
    }
    pub async fn targets_with_signal(
        &self,
        connection: Option<&str>,
        send: impl FnOnce(Value),
        signal: Option<maho_ai::utils::abort::AbortSignal>,
    ) -> Value {
        let Some(connection) = connection else {
            return serde_json::to_value(maho_core::session_relocation::relocation_not_offered())
                .expect("relocation result serializes");
        };
        let answer = self
            .ask(
                json!({"method":"list_relocation_targets"}),
                connection,
                send,
                signal,
            )
            .await;
        if answer.get("status").is_some() {
            return answer;
        }
        let Some(targets) = answer["data"]["targets"].as_array() else {
            return refused(
                "invalid_host_answer",
                "the app sent a list of computers this session cannot read",
            );
        };
        if targets.iter().any(|target| {
            target["name"].as_str().is_none_or(str::is_empty) || !target["online"].is_boolean()
        }) {
            return refused(
                "invalid_host_answer",
                "the app sent a list of computers this session cannot read",
            );
        }
        let targets = targets
            .iter()
            .map(|target| {
                let mut value = json!({"name":target["name"],"online":target["online"]});
                if target["capacity"].is_string() {
                    value["capacity"] = target["capacity"].clone();
                }
                if target["aboutToSleep"].is_boolean() {
                    value["aboutToSleep"] = target["aboutToSleep"].clone();
                }
                value
            })
            .collect::<Vec<_>>();
        json!({"status":"listed","targets":targets})
    }
    pub async fn request(
        &self,
        target: &str,
        reason: &str,
        connection: Option<&str>,
        saved: bool,
        send: impl FnOnce(Value),
    ) -> Value {
        self.request_with_signal(target, reason, connection, saved, send, None)
            .await
    }
    pub async fn request_with_signal(
        &self,
        target: &str,
        reason: &str,
        connection: Option<&str>,
        saved: bool,
        send: impl FnOnce(Value),
        signal: Option<maho_ai::utils::abort::AbortSignal>,
    ) -> Value {
        let Some(connection) = connection else {
            return serde_json::to_value(maho_core::session_relocation::relocation_not_offered())
                .expect("relocation result serializes");
        };
        if !saved {
            return json!({"status":"unavailable","reason":"not_releasable","detail":"this session is not saved to a file"});
        }
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.scheduled.is_some() || state.requesting {
                return refused(
                    "already_scheduled",
                    &format!(
                        "a move to {} is already set for the end of this turn",
                        state
                            .scheduled
                            .as_ref()
                            .map_or("another computer", |due| due.target.as_str())
                    ),
                );
            }
            state.requesting = true;
        }
        let answer = self
            .ask(
                json!({"method":"relocate_session","target":target,"reason":reason}),
                connection,
                send,
                signal,
            )
            .await;
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.requesting = false;
        if answer.get("status").is_some() {
            return answer;
        }
        let data = &answer["data"];
        if data["accepted"] == false {
            return refused(
                data["reason"]
                    .as_str()
                    .filter(|text| !text.is_empty())
                    .unwrap_or("declined"),
                data["detail"]
                    .as_str()
                    .filter(|text| !text.is_empty())
                    .unwrap_or("the app declined the move"),
            );
        }
        let id = data["moveId"].as_str().unwrap_or_default();
        if data["accepted"] != true || !is_plain_id(id) {
            return refused(
                "invalid_host_answer",
                "the app accepted the move without a usable move id",
            );
        }
        let target = data["target"]
            .as_str()
            .filter(|text| !text.is_empty())
            .unwrap_or(target)
            .to_owned();
        state.scheduled = Some(Scheduled {
            move_id: id.into(),
            target: target.clone(),
            reason: reason.into(),
            connection: connection.into(),
            aborted: false,
        });
        json!({"status":"scheduled","target":target,"moveId":id})
    }
    pub fn respond(&self, frame: &Value, connection: &str) -> HostResponseOutcome {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let id = frame["id"].as_str().unwrap_or_default();
        let Some(pending) = state.pending.get(id) else {
            return HostResponseOutcome::Unknown;
        };
        if pending.connection != connection {
            return HostResponseOutcome::NotOfferingConnection;
        }
        if let Some(pending) = state.pending.remove(id) {
            let answer = frame["error"].as_str().map_or_else(
                || json!({"data":frame["data"]}),
                |error| refused("host_error", error),
            );
            let _ = pending.finish.send(answer);
        }
        HostResponseOutcome::Answered
    }
    pub fn connection_detached(&self, connection: &str) -> Option<Value> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let ids = state
            .pending
            .iter()
            .filter(|(_, pending)| pending.connection == connection)
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in ids {
            if let Some(pending) = state.pending.remove(&id) {
                let _ = pending.finish.send(refused(
                    "app_disconnected",
                    "the app that was asked left this session",
                ));
            }
        }
        if state
            .scheduled
            .as_ref()
            .is_some_and(|due| due.connection == connection)
        {
            state.scheduled.take().map(|due| {
                let reason = if due.aborted {
                    "aborted"
                } else {
                    "disconnected"
                };
                drop_record(&due, reason)
            })
        } else {
            None
        }
    }
    pub fn agent_end(&self, user_aborted: bool) {
        if user_aborted
            && let Some(due) = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .scheduled
                .as_mut()
        {
            due.aborted = true;
        }
    }
    /// Called only after agent_settled and all admitted prompt calls have completed.
    pub fn announce_when_releasable(
        &self,
        offerer: Option<&str>,
        streaming: bool,
    ) -> Option<(String, Value)> {
        if streaming {
            return None;
        }
        let due = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .scheduled
            .take()?;
        let record = if due.aborted {
            drop_record(&due, "aborted")
        } else if offerer != Some(due.connection.as_str()) {
            drop_record(&due, "withdrawn")
        } else {
            json!({"type":"relocation_due","moveId":due.move_id,"target":due.target,"reason":due.reason})
        };
        Some((due.connection, record))
    }
    pub fn dispose(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.scheduled = None;
        for (_, pending) in std::mem::take(&mut state.pending) {
            let _ = pending
                .finish
                .send(refused("session_closed", "this session was closed"));
        }
    }
}
fn is_plain_id(id: &str) -> bool {
    let mut chars = id.chars();
    id.len() <= 128
        && chars.next().is_some_and(|ch| ch.is_ascii_alphanumeric())
        && chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
}
fn drop_record(due: &Scheduled, reason: &str) -> Value {
    json!({"customType":"session_relocation","content":format!("The session will not continue on {} ({}). The session stays on this computer; keep working here.",due.target,reason),"display":false,"details":{"status":"dropped","reason":reason,"moveId":due.move_id,"target":due.target}})
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn detach_refuses_pending_request_and_cancels_schedule() {
        let relocation = RpcSessionRelocation::default();
        let other = relocation.clone();
        let targets = relocation
            .targets(Some("app"), move |_| {
                other.connection_detached("app");
            })
            .await;
        assert_eq!(targets["reason"], "app_disconnected");
        let responder = relocation.clone();
        let answer = relocation
            .request("laptop", "continue", Some("app"), true, move |frame| {
                responder.respond(
                    &json!({"id":frame["id"],"data":{"accepted":true,"moveId":"m1"}}),
                    "app",
                );
            })
            .await;
        assert_eq!(answer["status"], "scheduled");
        assert_eq!(
            relocation.connection_detached("app").unwrap()["details"]["reason"],
            "disconnected"
        );
        assert!(
            relocation
                .announce_when_releasable(Some("app"), false)
                .is_none()
        );
    }
    #[tokio::test]
    async fn accepted_move_waits_for_settlement_and_user_abort_drops_it() {
        let relocation = RpcSessionRelocation::default();
        let responder = relocation.clone();
        let answer = relocation
            .request("laptop", "continue", Some("app"), true, move |frame| {
                responder.respond(
                    &json!({"id":frame["id"],"data":{"accepted":true,"moveId":"m1"}}),
                    "app",
                );
            })
            .await;
        assert_eq!(answer["moveId"], "m1");
        assert!(
            relocation
                .announce_when_releasable(Some("app"), true)
                .is_none()
        );
        relocation.agent_end(true);
        assert_eq!(
            relocation
                .announce_when_releasable(Some("app"), false)
                .unwrap()
                .1["details"]["reason"],
            "aborted"
        );
    }
    #[tokio::test]
    async fn cancellation_clears_pending_request() {
        let relocation = RpcSessionRelocation::default();
        let abort = maho_ai::utils::abort::AbortController::new();
        let signal = abort.signal();
        let answer = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            relocation.targets_with_signal(Some("app"), |_| abort.abort(None), Some(signal)),
        )
        .await
        .unwrap();
        assert_eq!(answer["reason"], "cancelled");
        assert!(relocation.state.lock().unwrap().pending.is_empty());
    }
    #[tokio::test]
    async fn only_offering_connection_can_answer() {
        let relocation = RpcSessionRelocation::default();
        let other = relocation.clone();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            relocation.targets(Some("app"), move |frame| {
                let reply =
                    json!({"id":frame["id"],"data":{"targets":[{"name":"laptop","online":true}]}});
                assert_eq!(
                    other.respond(&reply, "intruder"),
                    HostResponseOutcome::NotOfferingConnection
                );
                assert_eq!(other.respond(&reply, "app"), HostResponseOutcome::Answered);
            }),
        )
        .await
        .unwrap();
        assert_eq!(result["status"], "listed");
    }
}
