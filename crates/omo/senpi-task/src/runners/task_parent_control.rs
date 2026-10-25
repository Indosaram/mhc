//! `runners/task-parent-control.ts`: validated native task-parent commands.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex, PoisonError};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{code}")]
pub struct TaskParentControlError {
    pub code: String,
}
impl TaskParentControlError {
    pub fn new(code: &str) -> Self {
        Self { code: code.into() }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RunPauseOwner {
    pub parent_session_id: String,
    pub parent_pause_seq: u64,
    pub run_ref: RunRef,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunRef {
    pub kind: String,
    pub id: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ResumeRequest {
    pub parent_session_id: String,
    pub parent_pause_seq: u64,
    pub resume_id: String,
    pub child_seq_cutoff: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TaskParentCommand {
    SetClientInfo {
        width: u64,
        capabilities: Vec<String>,
    },
    PauseForRun {
        #[serde(flatten)]
        owner: RunPauseOwner,
    },
    ResumeForRun {
        #[serde(flatten)]
        request: ResumeRequest,
    },
    HoldInput {
        #[serde(rename = "inputId")]
        input_id: String,
        revision: u64,
        text: String,
        #[serde(rename = "parentSessionId")]
        parent_session_id: String,
        origin: String,
    },
    GetState,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldGuidanceInput {
    pub input_id: String,
    pub revision: u64,
    pub text: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineInheritReceipt {
    Held { child_seq: u64 },
    WrongParent,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineResumeReceipt {
    Refused {
        reason: String,
    },
    Released {
        other_hold_live: Option<bool>,
        turn_id: Option<String>,
    },
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineHoldInputResult {
    Held { entry_id: String, duplicate: bool },
    AlreadyDelivered { revision: u64 },
    InputIdConflict { held_revision: u64 },
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedHold {
    pub held: bool,
    pub child_seq: u64,
}

pub type TaskParentSender =
    Arc<dyn Fn(Value) -> Result<Value, TaskParentControlError> + Send + Sync>;
/// Share this wire between controls to identify a connection only once, including failed identification.
pub struct TaskParentWire {
    pub width: u64,
    pub send: TaskParentSender,
    identified: Mutex<Option<Result<(), TaskParentControlError>>>,
}
impl TaskParentWire {
    pub fn new(width: u64, send: TaskParentSender) -> Arc<Self> {
        Arc::new(Self {
            width,
            send,
            identified: Mutex::new(None),
        })
    }
}
pub struct TaskParentControl {
    wire: Arc<TaskParentWire>,
}
pub fn create_task_parent_control(wire: Arc<TaskParentWire>) -> TaskParentControl {
    TaskParentControl { wire }
}
fn rejection(message: &str) -> TaskParentControlError {
    TaskParentControlError::new(if message.starts_with("task_parent_required:") {
        "task_parent_required"
    } else if message.starts_with("Unknown command:") {
        "unsupported"
    } else {
        "request_rejected"
    })
}
fn error(response: &Value) -> &str {
    response.get("error").and_then(Value::as_str).unwrap_or("")
}
fn protocol() -> TaskParentControlError {
    TaskParentControlError::new("protocol_error")
}
impl TaskParentControl {
    fn identify(&self) -> Result<(), TaskParentControlError> {
        let mut ready = self
            .wire
            .identified
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        ready.get_or_insert_with(|| {
            let response = (self.wire.send)(json!({"type":"set_client_info", "width":self.wire.width, "capabilities":["task_parent"]}))?;
            if response["success"] == true { Ok(()) } else { Err(rejection(error(&response))) }
        }).clone()
    }
    pub fn pause_for_run(
        &self,
        owner: &RunPauseOwner,
    ) -> Result<EngineInheritReceipt, TaskParentControlError> {
        self.identify()?;
        let mut command = serde_json::to_value(owner).map_err(|_| protocol())?;
        command["type"] = json!("pause_for_run");
        let response = (self.wire.send)(command)?;
        if response["success"] != true {
            return if error(&response).starts_with("wrong_parent:") {
                Ok(EngineInheritReceipt::WrongParent)
            } else {
                Err(rejection(error(&response)))
            };
        }
        let data = &response["data"];
        let seq = data["childSeq"]
            .as_u64()
            .filter(|seq| *seq <= 9_007_199_254_740_991)
            .ok_or_else(protocol)?;
        if data["held"] != true {
            return Err(protocol());
        }
        Ok(EngineInheritReceipt::Held { child_seq: seq })
    }
    pub fn resume_for_run(
        &self,
        request: &ResumeRequest,
    ) -> Result<EngineResumeReceipt, TaskParentControlError> {
        self.identify()?;
        let mut command = serde_json::to_value(request).map_err(|_| protocol())?;
        command["type"] = json!("resume_for_run");
        let response = (self.wire.send)(command)?;
        if response["success"] != true {
            return if error(&response).starts_with("wrong_parent:") {
                Ok(EngineResumeReceipt::Refused {
                    reason: "wrong_parent".into(),
                })
            } else {
                Err(rejection(error(&response)))
            };
        }
        let data = &response["data"];
        if data["released"] == false
            && matches!(data["reason"].as_str(), Some("stale" | "not_held"))
        {
            return Ok(EngineResumeReceipt::Refused {
                reason: data["reason"].as_str().unwrap_or_default().into(),
            });
        }
        if data["released"] != true
            || data.get("otherHoldLive").is_some_and(|v| !v.is_boolean())
            || data.get("turnId").is_some_and(|v| !v.is_string())
        {
            return Err(protocol());
        }
        Ok(EngineResumeReceipt::Released {
            other_hold_live: data["otherHoldLive"].as_bool(),
            turn_id: data["turnId"].as_str().map(str::to_owned),
        })
    }
    pub fn hold_input(
        &self,
        parent_session_id: &str,
        input: &HeldGuidanceInput,
    ) -> Result<EngineHoldInputResult, TaskParentControlError> {
        self.identify()?;
        let response = (self.wire.send)(
            json!({"type":"hold_input", "inputId":input.input_id, "revision":input.revision, "text":input.text, "parentSessionId":parent_session_id, "origin":"human"}),
        )?;
        if response["success"] != true {
            let message = error(&response);
            if message.starts_with("input_id_conflict:")
                && let Some(revision) = message
                    .rsplit_once(" at revision ")
                    .and_then(|(_, r)| r.parse().ok())
                {
                    return Ok(EngineHoldInputResult::InputIdConflict {
                        held_revision: revision,
                    });
                }
            return Err(if message.starts_with("wrong_parent:") {
                TaskParentControlError::new("wrong_parent")
            } else {
                rejection(message)
            });
        }
        let data = &response["data"];
        if data["alreadyDelivered"] == true {
            return Ok(EngineHoldInputResult::AlreadyDelivered {
                revision: input.revision,
            });
        }
        Ok(EngineHoldInputResult::Held {
            entry_id: data["entryId"].as_str().ok_or_else(protocol)?.into(),
            duplicate: data["duplicate"].as_bool().ok_or_else(protocol)?,
        })
    }
    pub fn observe_hold(
        &self,
        parent_session_id: &str,
    ) -> Result<Option<ObservedHold>, TaskParentControlError> {
        let response = (self.wire.send)(json!({"type":"get_state"}))?;
        if response["success"] != true {
            return Ok(None);
        }
        let pause = &response["data"]["pause"];
        let (Some(child_seq), Some(records)) =
            (pause["pauseSeq"].as_u64(), pause["records"].as_array())
        else {
            return Ok(None);
        };
        Ok(Some(ObservedHold {
            child_seq,
            held: records.iter().any(|record| {
                record["kind"] == "inherited_hold" && record["parentSessionId"] == parent_session_id
            }),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn owner() -> RunPauseOwner {
        RunPauseOwner {
            parent_session_id: "parent".into(),
            parent_pause_seq: 2,
            run_ref: RunRef {
                kind: "dag".into(),
                id: "run".into(),
            },
        }
    }
    fn request() -> ResumeRequest {
        ResumeRequest {
            parent_session_id: "parent".into(),
            parent_pause_seq: 2,
            resume_id: "r".into(),
            child_seq_cutoff: 8,
        }
    }
    #[test]
    fn controls_share_identification_and_preserve_native_fields() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&calls);
        let wire = TaskParentWire::new(
            80,
            Arc::new(move |command| {
                recorded.lock().expect("calls").push(command.clone());
                Ok(match command["type"].as_str() {
                    Some("pause_for_run") => {
                        json!({"success":true,"data":{"held":true,"childSeq":8}})
                    }
                    Some("resume_for_run") => {
                        json!({"success":true,"data":{"released":true,"otherHoldLive":false,"turnId":"19"}})
                    }
                    _ => json!({"success":true}),
                })
            }),
        );
        let first = create_task_parent_control(Arc::clone(&wire));
        let second = create_task_parent_control(wire);
        assert_eq!(
            first.pause_for_run(&owner()),
            Ok(EngineInheritReceipt::Held { child_seq: 8 })
        );
        assert_eq!(
            second.resume_for_run(&request()),
            Ok(EngineResumeReceipt::Released {
                other_hold_live: Some(false),
                turn_id: Some("19".into())
            })
        );
        let calls = calls.lock().expect("calls");
        assert_eq!(calls.len(), 3);
        assert_eq!(
            calls[0],
            json!({"type":"set_client_info","width":80,"capabilities":["task_parent"]})
        );
        assert_eq!(calls[1]["runRef"], json!({"kind":"dag","id":"run"}));
        assert_eq!(
            calls[2],
            json!({"type":"resume_for_run","parentSessionId":"parent","parentPauseSeq":2,"resumeId":"r","childSeqCutoff":8})
        );
    }
    #[test]
    fn guidance_is_human_relay_and_preserves_delivery_and_conflicts() {
        for response in [
            json!({"success":true,"data":{"entryId":"held-1","duplicate":false}}),
            json!({"success":true,"data":{"alreadyDelivered":true}}),
            json!({"success":false,"error":"wrong_parent: other owner"}),
            json!({"success":false,"error":"input_id_conflict: input at revision 9"}),
        ] {
            let answer = response.clone();
            let control = create_task_parent_control(TaskParentWire::new(
                80,
                Arc::new(move |command| {
                    if command["type"] == "set_client_info" {
                        return Ok(json!({"success":true}));
                    }
                    assert_eq!(
                        command,
                        json!({"type":"hold_input","inputId":"guide-1","revision":2,"text":"Use staging","parentSessionId":"parent","origin":"human"})
                    );
                    Ok(answer.clone())
                }),
            ));
            let result = control.hold_input(
                "parent",
                &HeldGuidanceInput {
                    input_id: "guide-1".into(),
                    revision: 2,
                    text: "Use staging".into(),
                },
            );
            if response["data"]["alreadyDelivered"] == true {
                assert_eq!(
                    result,
                    Ok(EngineHoldInputResult::AlreadyDelivered { revision: 2 })
                );
            } else if response["data"]["entryId"].is_string() {
                assert_eq!(
                    result,
                    Ok(EngineHoldInputResult::Held {
                        entry_id: "held-1".into(),
                        duplicate: false
                    })
                );
            } else if response["error"]
                .as_str()
                .unwrap_or_default()
                .starts_with("wrong_parent:")
            {
                assert_eq!(result.unwrap_err().code, "wrong_parent");
            } else {
                assert_eq!(
                    result,
                    Ok(EngineHoldInputResult::InputIdConflict { held_revision: 9 })
                );
            }
        }
    }
    #[test]
    fn refusals_other_holds_and_protocol_errors_are_not_turns() {
        for (data, expected) in [
            (
                json!({"released":false,"reason":"stale"}),
                EngineResumeReceipt::Refused {
                    reason: "stale".into(),
                },
            ),
            (
                json!({"released":true,"otherHoldLive":true}),
                EngineResumeReceipt::Released {
                    other_hold_live: Some(true),
                    turn_id: None,
                },
            ),
        ] {
            let control = create_task_parent_control(TaskParentWire::new(
                80,
                Arc::new(move |_| Ok(json!({"success":true,"data":data}))),
            ));
            assert_eq!(control.resume_for_run(&request()), Ok(expected));
        }
        let control = create_task_parent_control(TaskParentWire::new(
            80,
            Arc::new(|command| {
                Ok(if command["type"] == "set_client_info" {
                    json!({"success":true})
                } else {
                    json!({"success":false,"error":"wrong_parent: other owner"})
                })
            }),
        ));
        assert_eq!(
            control.pause_for_run(&owner()),
            Ok(EngineInheritReceipt::WrongParent)
        );
        assert_eq!(
            control.resume_for_run(&request()),
            Ok(EngineResumeReceipt::Refused {
                reason: "wrong_parent".into()
            })
        );
        assert_eq!(
            rejection("Unknown command: pause_for_run").code,
            "unsupported"
        );
        assert_eq!(
            rejection("task_parent_required: identify").code,
            "task_parent_required"
        );
        let malformed = create_task_parent_control(TaskParentWire::new(
            80,
            Arc::new(|_| Ok(json!({"success":true,"data":{"held":true,"childSeq":-1}}))),
        ));
        assert_eq!(
            malformed.pause_for_run(&owner()).unwrap_err().code,
            "protocol_error"
        );
    }
}
