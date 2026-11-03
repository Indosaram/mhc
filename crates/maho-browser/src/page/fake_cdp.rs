//! In-memory CDP peer for unit tests: commands are answered by a handler closure, events are
//! pushed by the test, and [`FakeCdp::flush`] returns once every earlier event was handled by the
//! client (replies and events share one ordered channel), so no test waits on the clock.
use crate::cdp::{CdpClient, Reader, Writer};
use serde_json::{Value, json};
use std::io;
use std::sync::{Arc, Mutex, PoisonError, mpsc};

/// Answers one command: `(method, params, session)`.
pub type Handler = Box<dyn FnMut(&str, &Value, Option<&str>) -> Result<Value, String> + Send>;

/// One command the client sent.
#[derive(Clone, Debug, PartialEq)]
pub struct Call {
    pub method: String,
    pub params: Value,
    pub session: Option<String>,
}

pub struct FakeCdp {
    pub client: CdpClient,
    calls: Arc<Mutex<Vec<Call>>>,
    incoming: mpsc::Sender<Option<String>>,
}

impl FakeCdp {
    pub fn new(mut handler: Handler) -> Self {
        let (tx, rx) = mpsc::channel::<Option<String>>();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let replies = tx.clone();
        let log = Arc::clone(&calls);
        let writer: Writer = Box::new(move |raw| {
            let message: Value = serde_json::from_str(raw).map_err(io::Error::other)?;
            let id = message.get("id").cloned().unwrap_or(Value::Null);
            let method = message
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let session = message
                .get("sessionId")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let params = message.get("params").cloned().unwrap_or(Value::Null);
            log.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(Call {
                    method: method.clone(),
                    params: params.clone(),
                    session: session.clone(),
                });
            let reply = if method == "Fake.flush" {
                json!({"id": id, "result": {}})
            } else {
                match handler(&method, &params, session.as_deref()) {
                    Ok(result) => json!({"id": id, "result": result}),
                    Err(error) => json!({"id": id, "error": {"code": -32000, "message": error}}),
                }
            };
            replies
                .send(Some(reply.to_string()))
                .map_err(|_| io::Error::other("fake peer gone"))
        });
        let reader: Reader = Box::new(move || Ok(rx.recv().ok().flatten()));
        Self {
            client: CdpClient::new(writer, reader),
            calls,
            incoming: tx,
        }
    }

    /// Delivers an event to the client.
    pub fn emit(&self, method: &str, params: Value, session: Option<&str>) {
        let mut event = json!({"method": method, "params": params});
        if let Some(session) = session {
            event["sessionId"] = json!(session);
        }
        let _ = self.incoming.send(Some(event.to_string()));
    }

    /// Returns once every event emitted before this call has reached the client's event log.
    pub fn flush(&self) {
        let _ = self.client.send("Fake.flush", json!({}), None);
    }

    /// Ends the connection.
    pub fn disconnect(&self) {
        let _ = self.incoming.send(None);
    }

    pub fn calls(&self) -> Vec<Call> {
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|call| call.method != "Fake.flush")
            .cloned()
            .collect()
    }

    pub fn calls_of(&self, method: &str) -> Vec<Call> {
        self.calls()
            .into_iter()
            .filter(|call| call.method == method)
            .collect()
    }
}
