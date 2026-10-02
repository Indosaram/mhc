use std::{collections::BTreeMap, path::Path};
use serde_json::{Value, json};
use crate::{session_registry::{SessionEntry, CreateEntry, SESSION_REGISTRY_MAX_ENTRIES}, session_reattach::BindingStore, session_continuity::{ContinuityInput, Decision}};

#[derive(Default)]
pub struct SessionRegistry {
    pub entries: BTreeMap<String, SessionEntry>,
    pub bindings: BindingStore,
    pub last_decisions: BTreeMap<String, crate::session_observability::Observation>,
    pub reapers: BTreeMap<String, tokio::task::JoinHandle<()>>,
    generation: u64,
}
pub struct ResidentInput<'a> {
    pub session: &'a str, pub account: &'a str, pub model: &'a str,
    pub context: &'a Value, pub options: &'a Value, pub executable: &'a Path,
    pub environment: &'a BTreeMap<String, String>, pub auth_lane: &'a str,
    pub custom: &'a BTreeMap<String, String>, pub tool_note: Option<&'a str>,
    pub now: u64, pub transcript_available: bool,
    pub signal: Option<&'a maho_ai::utils::abort::AbortSignal>,
}
impl SessionRegistry {
    pub async fn close(&mut self, session: &str) -> anyhow::Result<()> {
        if let Some(reaper) = self.reapers.remove(session) { reaper.abort(); let _ = reaper.await; }
        if let Some(mut entry) = self.entries.remove(session) {
            self.bindings.remember(session, &crate::session_turn_attempt::binding_from_entry(&entry, &entry.sent_hashes));
            entry.close().await?;
        }
        Ok(())
    }
    pub async fn close_all(&mut self) -> anyhow::Result<()> {
        let sessions: Vec<_> = self.entries.keys().cloned().collect();
        for session in sessions { self.close(&session).await?; }
        Ok(())
    }
    pub async fn turn(&mut self, input: ResidentInput<'_>, deliver: impl FnMut(Value)) -> anyhow::Result<crate::session_registry_pump::TurnResult> {
        if let Some(reaper) = self.reapers.remove(input.session) { reaper.abort(); let _ = reaper.await; }
        let messages = crate::session_sync::sent_messages(input.context);
        let hashes = crate::session_sync::sent_message_hashes(&messages);
        let fingerprint = crate::session_sync::config_fingerprint(input.options, input.context, input.auth_lane, input.account);
        let snapshot = self.entries.get(input.session).map(|entry| crate::session_turn_attempt::binding_from_entry(entry, &entry.sent_hashes));
        let binding = self.bindings.get(input.session);
        let decision = crate::session_continuity::decide(&ContinuityInput { entry: snapshot.as_ref(), binding: binding.as_ref(), current_hashes: &hashes, account_name: input.account, model_id: input.model, system_prompt_hash: &fingerprint.system_prompt_hash, toolset_hash: &fingerprint.toolset_hash, transcript_available: input.transcript_available, cross_account_resume_supported: input.auth_lane != "config-dir", idle_expired: self.entries.get(input.session).is_some_and(|entry| entry.idle_expired(input.now)), invalidation_reason: self.bindings.invalidation_reason(input.session) });
        let (from, resume, sdk_id) = match &decision {
            Decision::Delta { from } => (*from, None, snapshot.as_ref().expect("live entry").sdk_session_id.clone()),
            Decision::Reattach { from, sdk_session_id, .. } => (*from, Some(None), sdk_session_id.clone()),
            Decision::Fork { from, sdk_session_id, at_uuid, .. } => (*from, Some(Some(at_uuid.as_str())), sdk_session_id.clone()),
            _ => (0, None, maho_ai::utils::uuid::uuidv7(None)?),
        };
        if !matches!(decision, Decision::Delta { .. }) {
            self.close(input.session).await?;
            if self.entries.len() >= SESSION_REGISTRY_MAX_ENTRIES {
                let oldest = self.entries.iter().filter(|(_, entry)| entry.evictable()).min_by_key(|(_, entry)| entry.last_used_at).map(|(session, _)| session.clone()).ok_or_else(|| anyhow::anyhow!("Anthropic Subscription registry capacity has no idle entry"))?;
                self.close(&oldest).await?;
            }
            self.generation += 1;
            let create = SessionEntry::create(CreateEntry { session_id: input.session, sdk_session_id: &sdk_id, account_name: input.account, model_id: input.model, system_prompt_hash: &fingerprint.system_prompt_hash, toolset_hash: &fingerprint.toolset_hash, executable: input.executable, options: input.options, environment: input.environment, generation: self.generation, now: input.now, resume_at: resume });
            let created = match input.signal {
                Some(signal) => maho_ai::utils::abort::race_with_abort_signal(create, signal).await.map_err(anyhow::Error::from).and_then(|result| result),
                None => create.await,
            };
            let mut entry = match created {
                Ok(entry) => entry,
                Err(error) => {
                    self.last_decisions.insert(input.session.into(), crate::session_observability::Observation { kind: "bootstrap".into(), reason: if input.signal.is_some_and(maho_ai::utils::abort::AbortSignal::aborted) { "resume_initialization_aborted" } else { "resume_initialization_failed" }.into(), delta_messages: hashes.len() - from, payload_bytes: None, collapsed_directives: None });
                    return Err(error);
                }
            };
            if let Some(source) = snapshot.as_ref().or(binding.as_ref()) {
                entry.assistant_uuid_by_index = source.assistant_uuid_by_index.iter().filter(|(index, _)| **index <= from).map(|(index, uuid)| (*index, uuid.clone())).collect();
                entry.sent_hashes = hashes[..from].to_vec();
            }
            self.entries.insert(input.session.into(), entry);
        }
        let blocks = if matches!(decision, Decision::Bootstrap { .. } | Decision::Flatten { .. }) {
            let blocks = crate::prompt_bridge::build_prompt_blocks(input.context["messages"].as_array().expect("messages"), input.custom, input.tool_note);
            crate::prompt_directive_dedupe::dedupe_ultrawork_blocks(&blocks).blocks
        } else { crate::session_sync::build_delta_prompt_blocks(&messages[from..], input.custom) };
        let entry = self.entries.get_mut(input.session).expect("admitted entry");
        let result = crate::session_turn_attempt::run(entry, maho_ai::utils::uuid::uuidv7(None)?, json!({"role":"user","content":blocks}), &hashes, (input.now, input.signal), &mut self.bindings, deliver).await;
        let (kind, reason, delta) = match &decision {
            Decision::Delta { from } => ("delta", "prefix_matched", hashes.len() - from),
            Decision::Reattach { from, reason, .. } | Decision::Fork { from, reason, .. } => ("fork", reason.as_str(), hashes.len() - from),
            Decision::Bootstrap { reason } => ("bootstrap", reason.as_deref().unwrap_or("registry_miss"), hashes.len()),
            Decision::Flatten { reason } => ("flatten", reason.as_str(), hashes.len()),
        };
        let cold = matches!(decision, Decision::Bootstrap { .. } | Decision::Flatten { .. });
        self.last_decisions.insert(input.session.into(), crate::session_observability::Observation { kind: if result.is_err() { "flatten" } else { kind }.into(), reason: result.as_ref().err().map_or_else(|| reason.into(), |error| crate::session_continuity::sanitize_reason(&error.to_string())), delta_messages: delta, payload_bytes: cold.then(|| crate::prompt_directive_dedupe::serialized_payload_bytes(&blocks)), collapsed_directives: None });
        if result.is_err() || self.entries.get(input.session).is_some_and(|entry| entry.pump.state == crate::session_registry_state::SessionState::Closed) {
            let checkpoint = self.entries.get(input.session).map(|entry| {
                let mut binding = crate::session_turn_attempt::binding_from_entry(entry, &entry.sent_hashes);
                binding.unanswered_turn_digest = Some(crate::session_sync::sent_hash_prefix_digest(&hashes, hashes.len())); binding
            });
            if let Some(mut entry) = self.entries.remove(input.session) { entry.close().await?; }
            if result.as_ref().err().is_some_and(|error| error.to_string().to_lowercase().contains("no conversation found with session id")) { self.bindings.forget(input.session); }
            else if let Some(checkpoint) = checkpoint { self.bindings.remember(input.session, &checkpoint); }
        }
        result
    }
}
impl Drop for SessionRegistry {
    fn drop(&mut self) { for reaper in self.reapers.values() { reaper.abort(); } }
}
