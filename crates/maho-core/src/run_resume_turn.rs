//! One-message resume delivery; held work is consumed only after successful admission.
use maho_ai::types::BoxFuture;
use serde_json::{Value, json};
use std::sync::Arc;
#[derive(Clone)]
pub struct HeldInput {
    pub input_id: String,
    pub text: String,
    pub images: Vec<Value>,
}
#[derive(Clone)]
pub struct HeldWake {
    pub source: String,
    pub message: Value,
}
pub trait RunResumePorts: Send + Sync {
    fn held_inputs(&self) -> Vec<HeldInput>;
    fn continue_inputs(&self) -> Vec<String>;
    fn held_wakes(&self) -> Vec<HeldWake>;
    fn send(&self, message: Value) -> BoxFuture<'_, Result<(), String>>;
    /// Subscribe before send; resolve None if send settles before a turn starts.
    fn turn_started(
        &self,
        until: tokio::sync::watch::Receiver<bool>,
    ) -> BoxFuture<'_, Option<String>>;
    fn idle(&self) -> BoxFuture<'_, ()>;
    fn current(&self) -> bool;
    fn consume(&self, input_ids: &[String], sources: &[String]);
    fn send_failed(&self, error: String);
}
pub struct RunResumeTurn {
    pub turn_id: Option<String>,
    pub delivered: BoxFuture<'static, bool>,
}
fn parts_of(content: &Value) -> Vec<Value> {
    match content {
        Value::String(text) => vec![json!({"type":"text","text":text})],
        Value::Array(parts) => parts.clone(),
        _ => Vec::new(),
    }
}
pub async fn start_run_resume_turn(ports: Arc<dyn RunResumePorts>) -> RunResumeTurn {
    let inputs = ports.held_inputs();
    let continues = ports.continue_inputs();
    let wakes = ports.held_wakes();
    let mut content = Vec::new();
    for input in &inputs {
        content.push(json!({"type":"text","text":format!("The user wrote while the run was paused:\n{}",input.text)}));
        content.extend(input.images.clone());
    }
    for wake in &wakes {
        content.extend(parts_of(&wake.message["content"]));
    }
    content.push(json!({"type":"text","text":"<system-reminder>The user stopped the previous turn and has now resumed. Tool calls that the Stop interrupted show an aborted result; they were not re-run and their side effects may be partial. Check their effects before repeating any of them, then continue the work.</system-reminder>"}));
    let (settled, until) = tokio::sync::watch::channel(false);
    let started = ports.turn_started(until);
    let message =
        json!({"customType":"session-resumed","content":content,"display":!inputs.is_empty()});
    let sending_ports = ports.clone();
    let sending = tokio::spawn(async move {
        let result = sending_ports.send(message).await;
        let _ = settled.send(true);
        result
    });
    let turn_id = started.await;
    let delivered_turn_id = turn_id.clone();
    let delivered = Box::pin(async move {
        ports.idle().await;
        match sending.await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                ports.send_failed(error);
                return false;
            }
            Err(error) => {
                ports.send_failed(error.to_string());
                return false;
            }
        }
        if delivered_turn_id.is_none() || !ports.current() {
            return false;
        }
        let mut ids: Vec<_> = inputs.into_iter().map(|i| i.input_id).collect();
        ids.extend(continues);
        ports.consume(
            &ids,
            &wakes.into_iter().map(|w| w.source).collect::<Vec<_>>(),
        );
        true
    });
    RunResumeTurn { turn_id, delivered }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Ports {
        consumed: std::sync::Mutex<Vec<String>>,
        started: tokio::sync::Notify,
        message: std::sync::Mutex<Option<Value>>,
        current: bool,
        fails: bool,
    }
    impl RunResumePorts for Ports {
        fn held_inputs(&self) -> Vec<HeldInput> {
            vec![HeldInput {
                input_id: "human".into(),
                text: "answer".into(),
                images: vec![],
            }]
        }
        fn continue_inputs(&self) -> Vec<String> {
            vec!["continue".into()]
        }
        fn held_wakes(&self) -> Vec<HeldWake> {
            vec![HeldWake {
                source: "monitor".into(),
                message: json!({"content":"wake"}),
            }]
        }
        fn send(&self, message: Value) -> BoxFuture<'_, Result<(), String>> {
            Box::pin(async move {
                *self.message.lock().expect("message") = Some(message);
                self.started.notify_one();
                if self.fails {
                    Err("disconnected".into())
                } else {
                    Ok(())
                }
            })
        }
        fn turn_started(
            &self,
            _: tokio::sync::watch::Receiver<bool>,
        ) -> BoxFuture<'_, Option<String>> {
            Box::pin(async move {
                self.started.notified().await;
                Some("turn".into())
            })
        }
        fn idle(&self) -> BoxFuture<'_, ()> {
            Box::pin(async {})
        }
        fn current(&self) -> bool {
            self.current
        }
        fn consume(&self, ids: &[String], _: &[String]) {
            *self.consumed.lock().expect("consumed") = ids.to_vec();
        }
        fn send_failed(&self, _: String) {}
    }
    #[tokio::test]
    async fn consumes_one_combined_delivery_only_after_success() {
        for (current, fails, delivered) in [
            (true, false, true),
            (false, false, false),
            (true, true, false),
        ] {
            let ports = Arc::new(Ports {
                consumed: Default::default(),
                started: tokio::sync::Notify::new(),
                message: Default::default(),
                current,
                fails,
            });
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                start_run_resume_turn(ports.clone()),
            )
            .await
            .expect("bounded turn");
            assert_eq!(
                tokio::time::timeout(std::time::Duration::from_secs(2), result.delivered)
                    .await
                    .expect("bounded delivery"),
                delivered
            );
            assert_eq!(
                ports.consumed.lock().expect("consumed").len(),
                if delivered { 2 } else { 0 }
            );
            let message = ports.message.lock().expect("message");
            let content = message.as_ref().expect("sent")["content"]
                .as_array()
                .expect("parts");
            assert_eq!(content.len(), 3);
            assert_eq!(content[1]["text"], "wake");
        }
    }
}
