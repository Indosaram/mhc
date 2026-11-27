//! Port of senpi `packages/coding-agent/src/modes/rpc/rpc-client-admissions.ts`
//! and `@earendil-works/pi-client` `recovery-repair.ts`.
//!
//! Pre-send admission routes and event-driven repair. No prompt, mutable-root fallback,
//! timer or retry loop.

use crate::rpc_command_error::RpcCommandError;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet, VecDeque};

const SENT_ADMISSION_IDS: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryPointer {
    pub token: String,
    pub component_ref: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactOpenResult {
    pub session_id: String,
    pub admission_id: String,
    pub generation: u64,
    pub recovery: RecoveryPointer,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryRepairSource {
    pub session_epoch: String,
    pub sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryRepairAdvice {
    pub state: RepairAdviceState,
    pub retry_after_ms: u64,
    pub health_revision: Option<u64>,
    pub source: Option<RecoveryRepairSource>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepairAdviceState {
    Ok,
    RepairNeeded,
    StorageUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryRepairAction {
    Ready,
    Wait { retry_after_ms: u64 },
    RestartFromCatalog,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid recovery repair advice")]
pub struct RecoveryRepairAdviceError;

pub fn parse_repair_advice(
    value: &Value,
) -> Result<RecoveryRepairAdvice, RecoveryRepairAdviceError> {
    let obj = value.as_object().ok_or(RecoveryRepairAdviceError)?;
    let state = match obj
        .get("state")
        .and_then(Value::as_str)
        .ok_or(RecoveryRepairAdviceError)?
    {
        "ok" => RepairAdviceState::Ok,
        "repair_needed" => RepairAdviceState::RepairNeeded,
        "storage_unavailable" => RepairAdviceState::StorageUnavailable,
        _ => return Err(RecoveryRepairAdviceError),
    };
    let retry_after_ms = obj
        .get("retryAfterMs")
        .and_then(Value::as_u64)
        .unwrap_or(100);
    let health_revision = obj.get("healthRevision").and_then(Value::as_u64);
    let source = obj.get("source").and_then(Value::as_object).and_then(|s| {
        Some(RecoveryRepairSource {
            session_epoch: s.get("sessionEpoch")?.as_str()?.to_owned(),
            sequence: s.get("sequence")?.as_u64()?,
        })
    });
    Ok(RecoveryRepairAdvice {
        state,
        retry_after_ms,
        health_revision,
        source,
    })
}

#[derive(Default)]
pub struct RecoveryRepairController {
    blocked: Option<(RecoveryRepairAdvice, u64)>,
    consecutive_gone: bool,
    gone_wait: Option<(String, u64)>,
    restarted_tokens: VecDeque<String>,
    restarted_set: HashSet<String>,
}

impl RecoveryRepairController {
    fn remember_restart(&mut self, token: &str) {
        if self.restarted_set.insert(token.to_owned()) {
            self.restarted_tokens.push_back(token.to_owned());
            if self.restarted_tokens.len() > 64
                && let Some(oldest) = self.restarted_tokens.pop_front()
            {
                self.restarted_set.remove(&oldest);
            }
        }
    }

    pub fn observe(
        &mut self,
        input: &Value,
        now_ms: u64,
        token: Option<&str>,
        reason: &str,
    ) -> Result<RecoveryRepairAction, RecoveryRepairAdviceError> {
        let advice = parse_repair_advice(input)?;
        if advice.state == RepairAdviceState::RepairNeeded
            && let Some(token) = token
            && (reason == "unknown_token" || reason == "released")
        {
            let delay = advice.retry_after_ms.max(100);
            self.blocked = None;
            if self.restarted_set.contains(token) {
                return Ok(RecoveryRepairAction::Wait {
                    retry_after_ms: delay,
                });
            }
            if !self.consecutive_gone {
                self.consecutive_gone = true;
                self.remember_restart(token);
                return Ok(RecoveryRepairAction::RestartFromCatalog);
            }
            let wait_until = match &self.gone_wait {
                Some((t, until)) if t == token => *until,
                _ => {
                    let until = now_ms.saturating_add(delay);
                    self.gone_wait = Some((token.to_owned(), until));
                    until
                }
            };
            if now_ms >= wait_until {
                self.remember_restart(token);
                self.gone_wait = None;
                return Ok(RecoveryRepairAction::RestartFromCatalog);
            }
            return Ok(RecoveryRepairAction::Wait {
                retry_after_ms: wait_until.saturating_sub(now_ms).max(100),
            });
        }
        if advice.state == RepairAdviceState::Ok {
            self.consecutive_gone = false;
            self.gone_wait = None;
        }
        if let Some((before, until)) = &self.blocked {
            let changed = (advice.health_revision.is_some()
                && advice.health_revision != before.health_revision)
                || (advice.source.is_some() && advice.source != before.source);
            if changed && now_ms >= *until {
                self.blocked = None;
                return Ok(RecoveryRepairAction::RestartFromCatalog);
            }
            return Ok(RecoveryRepairAction::Wait {
                retry_after_ms: until.saturating_sub(now_ms).max(100),
            });
        }
        match advice.state {
            RepairAdviceState::Ok => Ok(RecoveryRepairAction::Ready),
            RepairAdviceState::RepairNeeded | RepairAdviceState::StorageUnavailable => {
                let retry_after_ms = advice.retry_after_ms.max(100);
                self.blocked = Some((advice, now_ms.saturating_add(retry_after_ms)));
                Ok(RecoveryRepairAction::Wait { retry_after_ms })
            }
        }
    }
}

struct Route {
    repair: RecoveryRepairController,
    session_id: Option<String>,
    token: Option<String>,
    generation: u64,
    not_before: u64,
}

pub enum PageStepOutcome {
    Ready {
        response: Value,
    },
    Wait {
        retry_after_ms: u64,
    },
    RestartFromCatalog {
        response: Value,
        identity: CompactOpenResult,
    },
}

pub struct RpcClientAdmissions {
    routes: HashMap<String, Route>,
    sent_order: VecDeque<String>,
    sent_set: HashSet<String>,
    serial: u64,
}

impl Default for RpcClientAdmissions {
    fn default() -> Self {
        Self::new()
    }
}

impl RpcClientAdmissions {
    pub fn new() -> Self {
        Self {
            routes: HashMap::new(),
            sent_order: VecDeque::new(),
            sent_set: HashSet::new(),
            serial: 0,
        }
    }

    pub fn clear(&mut self) {
        self.routes.clear();
    }

    /// Records a send of `admission_id`; returns `true` if this client already sent it (C4.4 retry).
    pub fn remember(&mut self, admission_id: &str) -> bool {
        let retry = self.sent_set.remove(admission_id);
        if retry {
            self.sent_order.retain(|id| id != admission_id);
        }
        self.sent_set.insert(admission_id.to_owned());
        self.sent_order.push_back(admission_id.to_owned());
        while self.sent_set.len() > SENT_ADMISSION_IDS {
            if let Some(oldest) = self.sent_order.pop_front() {
                self.sent_set.remove(&oldest);
            }
        }
        retry
    }

    pub fn receive(&mut self, input: &Value) {
        if self.routes.is_empty() {
            return;
        }
        let Some(obj) = input.as_object() else {
            return;
        };
        let (Some(session_id), Some(admission_id), Some(generation)) = (
            obj.get("sessionId").and_then(Value::as_str),
            obj.get("admissionId").and_then(Value::as_str),
            obj.get("generation").and_then(Value::as_u64),
        ) else {
            return;
        };
        let Some(route) = self.routes.get_mut(admission_id) else {
            return;
        };
        if generation < route.generation {
            return;
        }
        route.session_id = Some(session_id.to_owned());
        route.generation = generation;
        if let Some(token) = obj.get("token").and_then(Value::as_str) {
            route.token = Some(token.to_owned());
        }
    }

    pub fn prepare_open(&mut self, admission_id: &str) -> bool {
        if !self.routes.contains_key(admission_id) {
            self.routes.insert(
                admission_id.to_owned(),
                Route {
                    repair: RecoveryRepairController::default(),
                    session_id: None,
                    token: None,
                    generation: 0,
                    not_before: 0,
                },
            );
        }
        self.remember(admission_id)
    }

    pub fn rollback_new_route(&mut self, admission_id: &str, existed_before: bool) {
        if !existed_before {
            self.routes.remove(admission_id);
        }
    }

    pub fn has_route(&self, admission_id: &str) -> bool {
        self.routes.contains_key(admission_id)
    }

    pub fn accept_open_or_reset(
        &mut self,
        response: &Value,
    ) -> Result<CompactOpenResult, RpcCommandError> {
        if response.get("success").and_then(Value::as_bool) != Some(true) {
            let msg = response
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("request failed");
            let code = response
                .get("errorCode")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let data = response.get("errorData").cloned();
            return Err(RpcCommandError::new(msg, code, data));
        }
        let session_id = response
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcCommandError::new("missing sessionId", None, None))?
            .to_owned();
        let admission_id = response
            .get("admissionId")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcCommandError::new("missing admissionId", None, None))?
            .to_owned();
        let generation = response
            .get("generation")
            .and_then(Value::as_u64)
            .ok_or_else(|| RpcCommandError::new("missing generation", None, None))?;
        let recovery = &response["data"]["recovery"];
        let token = recovery
            .get("token")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcCommandError::new("missing recovery.token", None, None))?
            .to_owned();
        let component_ref = recovery
            .get("componentRef")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcCommandError::new("missing recovery.componentRef", None, None))?
            .to_owned();
        self.receive(&json!({
            "sessionId": session_id,
            "admissionId": admission_id,
            "generation": generation,
            "token": token,
        }));
        let route = self.routes.get(&admission_id);
        Ok(CompactOpenResult {
            session_id: route
                .and_then(|r| r.session_id.clone())
                .unwrap_or(session_id),
            admission_id,
            generation: route.map_or(generation, |r| r.generation),
            recovery: RecoveryPointer {
                token: route.and_then(|r| r.token.clone()).unwrap_or(token),
                component_ref,
            },
        })
    }

    pub fn build_page_command(
        &mut self,
        admission_id: &str,
        component_ref: &str,
        cursor: Option<&str>,
        now_ms: u64,
    ) -> Result<Result<Value, u64>, RpcCommandError> {
        let route = self.routes.get(admission_id).ok_or_else(|| {
            RpcCommandError::new(
                "unknown compact admission",
                Some("unknown_admission".to_owned()),
                Some(json!({"code": "unknown_admission", "admissionId": admission_id})),
            )
        })?;
        let (Some(token), Some(session_id)) = (&route.token, &route.session_id) else {
            return Err(RpcCommandError::new(
                "unknown compact admission",
                Some("unknown_admission".to_owned()),
                Some(json!({"code": "unknown_admission", "admissionId": admission_id})),
            ));
        };
        if now_ms < route.not_before {
            return Ok(Err(route.not_before - now_ms));
        }
        self.serial += 1;
        let mut cmd = json!({
            "type": "get_recovery_page",
            "sessionId": session_id,
            "admissionId": admission_id,
            "token": token,
            "componentRef": component_ref,
            "requestId": format!("page-{}", self.serial),
        });
        if let Some(cursor) = cursor {
            cmd["cursor"] = json!(cursor);
        }
        Ok(Ok(cmd))
    }

    pub fn observe_page_response(
        &mut self,
        admission_id: &str,
        sent_token: &str,
        response: &Value,
        now_ms: u64,
    ) -> Result<RecoveryRepairAction, RpcCommandError> {
        let ok = response.get("success").and_then(Value::as_bool) == Some(true);
        let payload = if ok {
            response.get("data")
        } else {
            response.get("errorData")
        };
        let Some(repair_val) = payload.and_then(|p| p.get("repair")) else {
            if !ok {
                return Err(RpcCommandError::new(
                    response
                        .get("error")
                        .and_then(Value::as_str)
                        .unwrap_or("page failed"),
                    response
                        .get("errorCode")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    response.get("errorData").cloned(),
                ));
            }
            return Err(RpcCommandError::new("missing repair advice", None, None));
        };
        let reason = payload
            .and_then(|p| p.get("reason"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let route = self.routes.get_mut(admission_id).ok_or_else(|| {
            RpcCommandError::new(
                "unknown compact admission",
                Some("unknown_admission".to_owned()),
                None,
            )
        })?;
        let action = route
            .repair
            .observe(repair_val, now_ms, Some(sent_token), reason)
            .map_err(|_| RpcCommandError::new("invalid recovery repair advice", None, None))?;
        if let RecoveryRepairAction::Wait { retry_after_ms } = action {
            route.not_before = now_ms.saturating_add(retry_after_ms);
        }
        Ok(action)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remember_marks_second_send_as_retry() {
        let mut admissions = RpcClientAdmissions::new();
        assert!(!admissions.remember("adm-1"));
        assert!(admissions.remember("adm-1"));
    }

    #[test]
    fn repair_controller_restarts_once_on_unknown_token() {
        let mut ctrl = RecoveryRepairController::default();
        let advice =
            json!({"state":"repair_needed","retryAfterMs":200,"healthRevision":null,"source":null});
        let a1 = ctrl
            .observe(&advice, 1000, Some("tok-1"), "unknown_token")
            .expect("ok");
        assert_eq!(a1, RecoveryRepairAction::RestartFromCatalog);
        let a2 = ctrl
            .observe(&advice, 1050, Some("tok-1"), "unknown_token")
            .expect("ok");
        assert_eq!(
            a2,
            RecoveryRepairAction::Wait {
                retry_after_ms: 200
            }
        );
    }
}
