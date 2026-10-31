//! Ports `runners/input-admission-read.test.ts`.

use std::sync::{Mutex, PoisonError};

use serde_json::{Value, json};

use super::*;
use crate::host::HostError;
use crate::manager::child_handle::{ManagedChildListener, Unsubscribe};
use crate::runners::rpc::protocol_client::{RpcClientError, RpcEventListener, RpcExitListener};
use crate::runners::types::TerminateOptions;

fn ids(count: usize) -> Vec<String> {
    (0..count).map(|index| format!("ps-{index}")).collect()
}

struct ReadPort {
    batches: Mutex<Vec<usize>>,
    answer: Box<dyn Fn(&[String]) -> Option<Value> + Send + Sync>,
}

impl HostSessionPort for ReadPort {
    fn send(&self, _command: Value) -> Result<(), HostError> {
        Ok(())
    }
    fn get_state(&self) -> Result<Value, HostError> {
        Ok(Value::Null)
    }
    fn on_event(&self, _listener: ManagedChildListener) -> Unsubscribe {
        Box::new(|| {})
    }
    fn close(&self) -> Result<(), HostError> {
        Ok(())
    }
    fn detach(&self) -> Result<(), HostError> {
        Ok(())
    }
    fn get_pending_inputs(&self, known: &[String]) -> Result<Option<Value>, HostError> {
        self.batches
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(known.len());
        Ok((self.answer)(known))
    }
}

#[test]
fn every_id_is_asked_once_in_batches_of_at_most_64() {
    let port = ReadPort {
        batches: Mutex::new(Vec::new()),
        answer: Box::new(|batch| {
            let outcomes: Vec<Value> = batch
                .iter()
                .map(|id| {
                    let status = if id == "ps-70" {
                        "admitted"
                    } else {
                        "not_admitted"
                    };
                    json!({ "clientMessageId": id, "status": status })
                })
                .collect();
            Some(
                json!({ "lifetimeId": "l-1", "throughCursor": 1, "inputs": [], "outcomes": outcomes }),
            )
        }),
    };
    let admissions = read_host_admissions(&port, &ids(130))
        .expect("read")
        .expect("supported");
    assert_eq!(
        *port.batches.lock().unwrap_or_else(PoisonError::into_inner),
        vec![64, 64, 2]
    );
    assert_eq!(admissions.get("ps-70"), Some(&InputAdmission::Admitted));
    assert_eq!(admissions.get("ps-0"), Some(&InputAdmission::NotAdmitted));
    assert_eq!(admissions.len(), 130);
}

#[test]
fn a_host_port_without_the_read_is_unsupported_not_a_failure() {
    let port = ReadPort {
        batches: Mutex::new(Vec::new()),
        answer: Box::new(|_| None),
    };
    assert_eq!(read_host_admissions(&port, &ids(1)), Ok(None));
}

#[test]
fn an_answer_that_leaves_an_id_out_fails() {
    let port = ReadPort {
        batches: Mutex::new(Vec::new()),
        answer: Box::new(|_| {
            Some(json!({ "outcomes": [{ "clientMessageId": "ps-0", "status": "admitted" }] }))
        }),
    };
    let error = read_host_admissions(&port, &ids(2)).expect_err("left out");
    assert!(error.contains("left out 1 id"), "{error}");
}

struct StdioClient {
    sent: Mutex<Vec<Value>>,
    answer: Box<dyn Fn() -> Result<Value, RpcClientError> + Send + Sync>,
}

impl RpcClientPort for StdioClient {
    fn pid(&self) -> Option<u32> {
        None
    }
    fn send(&self, command: Value) -> Result<Value, RpcClientError> {
        self.sent
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(command);
        (self.answer)()
    }
    fn on_event(&self, _listener: RpcEventListener) -> Unsubscribe {
        Box::new(|| {})
    }
    fn on_exit(&self, _listener: RpcExitListener) -> Unsubscribe {
        Box::new(|| {})
    }
    fn stderr_tail(&self) -> String {
        String::new()
    }
    fn detach(&self) {}
    fn terminate(&self, _options: TerminateOptions) -> std::io::Result<()> {
        Ok(())
    }
}

fn stdio(
    answer: impl Fn() -> Result<Value, RpcClientError> + Send + Sync + 'static,
) -> StdioClient {
    StdioClient {
        sent: Mutex::new(Vec::new()),
        answer: Box::new(answer),
    }
}

#[test]
fn a_stdio_child_answers_each_ids_admission() {
    let client = stdio(|| {
        Ok(json!({
            "type": "response", "command": "get_pending_inputs", "success": true,
            "data": { "lifetimeId": "l-1", "throughCursor": 2, "inputs": [],
                "outcomes": [{ "clientMessageId": "ps-0", "status": "admitted" }] },
        }))
    });
    let admissions = read_stdio_admissions(&client, &ids(1))
        .expect("read")
        .expect("supported");
    assert_eq!(
        *client.sent.lock().unwrap_or_else(PoisonError::into_inner),
        vec![json!({ "type": "get_pending_inputs", "knownClientMessageIds": ["ps-0"] })]
    );
    assert_eq!(admissions.get("ps-0"), Some(&InputAdmission::Admitted));
}

#[test]
fn an_unknown_command_refusal_is_unsupported_and_a_lost_answer_fails() {
    let refused = stdio(|| {
        Ok(
            json!({ "type": "response", "command": "get_pending_inputs", "success": false, "error": "Unknown command" }),
        )
    });
    assert_eq!(read_stdio_admissions(&refused, &ids(1)), Ok(None));
    let lost = stdio(|| {
        Err(RpcClientError::Write {
            message: "pipe closed".into(),
        })
    });
    let error = read_stdio_admissions(&lost, &ids(1)).expect_err("lost");
    assert!(error.contains("pipe closed"));
}

#[test]
fn any_other_refusal_fails_the_read() {
    let client = stdio(|| {
        Ok(
            json!({ "type": "response", "command": "get_pending_inputs", "success": false, "error": "session is closing" }),
        )
    });
    let error = read_stdio_admissions(&client, &ids(1)).expect_err("refused");
    assert!(error.contains("session is closing"));
}
