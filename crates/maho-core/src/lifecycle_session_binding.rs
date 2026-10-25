//! Durable lifecycle binding at session admission and event boundaries.
use crate::agent_session::{AgentSession, AgentSessionSubscription};
use crate::event_bus::EventSubscription;
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
/// Host adapter for the lifecycle controller supplied by the owning session runtime.
pub trait LifecycleBindingHost: Send + Sync {
    fn event(&self, name: &str, data: Value);
    fn facts_failed(&self, error: String);
}
pub struct LifecycleSessionBinding {
    pub session_id: String,
    session: Arc<AgentSession>,
    host: Arc<dyn LifecycleBindingHost>,
    subscriptions: Vec<EventSubscription>,
    session_subscription: Option<AgentSessionSubscription>,
    unsubscribe_human: Option<Box<dyn FnOnce() + Send>>,
    refresh_generation: AtomicU64,
    restored: bool,
    next_run_origin: Option<String>,
}
impl Drop for LifecycleSessionBinding {
    fn drop(&mut self) {
        self.dispose();
    }
}
impl LifecycleSessionBinding {
    pub fn new(session: Arc<AgentSession>, host: Arc<dyn LifecycleBindingHost>) -> Self {
        let session_id = session.session_id();
        let event_host = host.clone();
        let session_subscription = session.subscribe(Arc::new(move |event| match event {
            maho_ext_api::AgentSessionEvent::Agent(maho_agent::types::AgentEvent::AgentStart) => {
                event_host.event("start_run", Value::Null)
            }
            maho_ext_api::AgentSessionEvent::AgentEnd { aborted, .. } => {
                event_host.event("turn_ended", json!({"aborted":aborted}))
            }
            _ => {}
        }));
        let human_host = host.clone();
        let unsubscribe_human = Some(Box::new(session.on_human_input_admitted(Arc::new(
            move |event| human_host.event("human_input", json!({"at":event.at})),
        ))) as Box<dyn FnOnce() + Send>);
        let mut binding = Self {
            session_id,
            session,
            host,
            subscriptions: Vec::new(),
            session_subscription: Some(session_subscription),
            unsubscribe_human,
            refresh_generation: AtomicU64::new(0),
            restored: false,
            next_run_origin: None,
        };
        binding.bind_fact_events();
        binding
    }
    pub fn bind_fact_events(&mut self) {
        self.subscriptions.clear();
        for channel in [
            "wake_source_state",
            "goal_continuation_timer_state",
            "goal_store_changed",
            "human_input_admitted",
            "inputs_settled",
        ] {
            let host = self.host.clone();
            let session_id = self.session_id.clone();
            self.subscriptions.push(self.session.on_extension_event(
                channel,
                Arc::new(move |data| {
                    if channel == "goal_store_changed"
                        && data.get("threadId").and_then(Value::as_str) != Some(&session_id)
                    {
                        return;
                    }
                    if channel == "wake_source_state"
                        && !(data.get("source").is_some_and(Value::is_string)
                            && data
                                .get("activeCount")
                                .and_then(Value::as_f64)
                                .is_some_and(f64::is_finite))
                    {
                        return;
                    }
                    host.event(channel, data.clone());
                }),
            ));
        }
    }
    pub fn prepare_run(&mut self, origin: Option<String>) {
        self.next_run_origin = origin;
    }
    pub fn is_restored(&self) -> bool {
        self.restored
    }
    pub fn start_run(&mut self) {
        self.host
            .event("start_run", json!({"origin":self.next_run_origin.take()}));
    }
    pub fn refresh_facts(&self, facts: Value) {
        self.refresh_generation.fetch_add(1, Ordering::SeqCst);
        self.host.event("goal_facts", facts);
    }
    pub fn consumed_entry(&self, message: &Value) -> Option<String> {
        if message["role"] != "custom" {
            return None;
        }
        let ticket = message.pointer("/details/ticketId")?.as_str()?;
        self.session.with_session_manager(|manager| {
            manager
                .entries()
                .iter()
                .find(|entry| {
                    entry["type"] == "custom_message"
                        && entry.pointer("/details/ticketId").and_then(Value::as_str)
                            == Some(ticket)
                })
                .and_then(|entry| entry["id"].as_str().map(str::to_owned))
        })
    }
    pub fn consume(&self, message: &Value, run_id: &str) -> Option<String> {
        if message["role"] != "custom" {
            return None;
        }
        let ticket = message.pointer("/details/ticketId")?.as_str()?;
        self.session.with_session_manager_mut(|manager| {
            let entries = manager.entries();
            let record = entries.iter().rev().find(|entry| {
                entry["customType"] == "work-custody"
                    && entry.pointer("/data/ticketId").and_then(Value::as_str) == Some(ticket)
            })?;
            if record.pointer("/data/state").and_then(Value::as_str) == Some("consumed") {
                return None;
            }
            let existing = entries
                .iter()
                .find(|entry| {
                    entry["type"] == "custom_message"
                        && entry.pointer("/details/ticketId").and_then(Value::as_str)
                            == Some(ticket)
                })
                .and_then(|entry| entry["id"].as_str())
                .map(str::to_owned);
            let transcript = existing.unwrap_or_else(|| {
                manager.append_custom_message(
                    message["customType"].as_str().unwrap_or_default(),
                    message["content"].clone(),
                    message["display"] == true,
                    message.get("details").cloned(),
                )["id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned()
            });
            let mut record = record["data"].clone();
            record["state"] = json!("consumed");
            record["runId"] = json!(run_id);
            record["transcriptEntryId"] = json!(transcript);
            manager.append_custom("work-custody", Some(record));
            Some(transcript)
        })
    }
    pub fn idle(&self) {
        self.host.event("idle", Value::Null);
    }
    /// Recover consumed deliveries without executing any tool a second time.
    pub async fn restore(&mut self) -> Result<(), String> {
        if self.restored {
            return Ok(());
        }
        self.session.with_session_manager_mut(|manager| {
            let entries = manager.entries();
            let mut custody = std::collections::BTreeMap::new();
            for entry in &entries {
                if entry["customType"] == "work-custody" && entry.pointer("/data/sessionId").and_then(Value::as_str) == Some(self.session_id.as_str())
                    && let Some(key) = entry.pointer("/data/key").and_then(Value::as_str) { custody.insert(key.to_owned(), entry["data"].clone()); }
            }
            for record in custody.values() {
                let Some(ticket) = record["ticketId"].as_str() else { continue };
                if !matches!(record["state"].as_str(), Some("open" | "queued" | "consumed")) { continue; }
                let transcript = entries.iter().find(|entry| entry["type"] == "custom_message" && entry.pointer("/details/ticketId").and_then(Value::as_str) == Some(ticket));
                if record["state"] != "consumed" && transcript.is_none() { continue; }
                let transcript_id = transcript.and_then(|entry| entry["id"].as_str()).or_else(|| record["transcriptEntryId"].as_str());
                if record["state"] != "consumed" { let mut consumed = record.clone(); consumed["state"] = json!("consumed"); consumed["transcriptEntryId"] = json!(transcript_id); manager.append_custom("work-custody", Some(consumed)); }
                if !entries.iter().any(|entry| entry["customType"] == "interrupted-run" && entry.pointer("/data/ticketId").and_then(Value::as_str) == Some(ticket)) { manager.append_custom("interrupted-run", Some(json!({"ticketId":ticket,"runId":record["runId"],"status":"INTERRUPTED","reason":"host_restart","at":chrono::Utc::now().to_rfc3339()}))); }
                if let Some(transcript_id) = transcript_id { let latest = manager.entries(); record_interrupted_tools(manager, &latest, transcript_id); }
            }
        });
        self.restored = true;
        self.host.event("restore_complete", Value::Null);
        let queued = self.session.with_session_manager(|manager| {
            let mut records = std::collections::BTreeMap::new();
            for entry in manager.entries() {
                if entry["customType"] == "work-custody"
                    && entry.pointer("/data/sessionId").and_then(Value::as_str)
                        == Some(self.session_id.as_str())
                    && let Some(key) = entry.pointer("/data/key").and_then(Value::as_str) {
                        records.insert(key.to_owned(), entry["data"].clone());
                    }
            }
            records
                .into_values()
                .filter(|record| record["state"] == "queued" && record["payload"].is_object())
                .collect::<Vec<_>>()
        });
        for record in queued {
            let message = &record["payload"]["message"];
            let content = serde_json::from_value(message["content"].clone())
                .map_err(|error| error.to_string())?;
            let mut details = message["details"].as_object().cloned().unwrap_or_default();
            details.insert("ticketId".into(), record["ticketId"].clone());
            let deliver_as = match record.pointer("/payload/deliverAs").and_then(Value::as_str) {
                Some("steer") => Some(maho_ext_api::DeliverAs::Steer),
                Some("followUp") => Some(maho_ext_api::DeliverAs::FollowUp),
                Some("nextTurn") => Some(maho_ext_api::DeliverAs::NextTurn),
                _ => None,
            };
            self.session
                .send_custom_message_content(
                    message["customType"]
                        .as_str()
                        .ok_or("invalid queued custom type")?
                        .into(),
                    content,
                    message["display"] == true,
                    Some(Value::Object(details)),
                    maho_ext_api::SendMessageOptions {
                        trigger_turn: true,
                        deliver_as,
                    },
                )
                .await?;
        }
        Ok(())
    }
    pub fn dispose(&mut self) {
        self.refresh_generation.fetch_add(1, Ordering::SeqCst);
        self.subscriptions.clear();
        self.session_subscription.take();
        if let Some(unsubscribe) = self.unsubscribe_human.take() {
            unsubscribe();
        }
    }
}
pub fn record_interrupted_tools(
    manager: &mut crate::session_manager::SessionManager,
    entries: &[Value],
    transcript_entry_id: &str,
) {
    let Some(start) = entries
        .iter()
        .position(|entry| entry["id"] == transcript_entry_id)
    else {
        return;
    };
    let mut pending: Vec<(String, String)> = Vec::new();
    for entry in &entries[start..] {
        if entry["type"] != "message" {
            continue;
        }
        let message = &entry["message"];
        if message["role"] == "assistant" {
            for part in message["content"].as_array().into_iter().flatten() {
                if part["type"] == "toolCall"
                    && let (Some(id), Some(name)) = (part["id"].as_str(), part["name"].as_str()) {
                        pending.retain(|(key, _)| key != id);
                        pending.push((id.into(), name.into()));
                    }
            }
        } else if message["role"] == "toolResult" {
            pending.retain(|(id, _)| Some(id.as_str()) != message["toolCallId"].as_str());
        }
    }
    for (id, name) in pending {
        manager.append_message(json!({"role":"toolResult","toolCallId":id,"toolName":name,"content":[{"type":"text","text":"INTERRUPTED: the consuming run ended before this tool result was recorded. Side effects may be partial. The tool was not re-run."}],"details":{"interrupted":{"reason":"host_restart"}},"isError":true,"timestamp":chrono::Utc::now().timestamp_millis()}));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recovery_records_only_unfinished_calls_and_never_replays_tools() {
        let mut manager = crate::session_manager::SessionManager::in_memory("/tmp", None, None);
        let start = manager.append_message(json!({"role":"user","content":"wake"}));
        manager.append_message(json!({"role":"assistant","content":[{"type":"toolCall","id":"done","name":"read"},{"type":"toolCall","id":"pending","name":"write"}]}));
        manager.append_message(json!({"role":"toolResult","toolCallId":"done","content":[]}));
        let entries = manager.entries();
        record_interrupted_tools(&mut manager, &entries, start["id"].as_str().expect("id"));
        let after = manager.entries();
        let last = after.last().expect("result");
        assert_eq!(last["message"]["toolCallId"], "pending");
        assert_eq!(last["message"]["isError"], true);
        record_interrupted_tools(&mut manager, &after, start["id"].as_str().expect("id"));
        assert_eq!(manager.entries().len(), after.len());
    }
}
