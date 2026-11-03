//! [`EffectSession`] over a live `maho_core::agent_session::AgentSession`: the view the effect ledger and the
//! client-admission ledger read and write when a terminal (or any in-process host) serves effect commands
//! (senpi reads `AgentSession` directly). [`AgentEffectSession::observe`] forwards the session events the
//! ledgers react to, narrowed to [`EffectSessionEvent`].
//!
//! Gaps against upstream, named where they sit: model and thinking changes carry no `effectId`, so a `set_model`
//! effect settles from the call's own result (the ledger's `ModelChanged` correlation never fires);
//! `restore_queued_input` re-queues text and images only, under a fresh `queuedId`.
use crate::client_admission_record::{PreparedAdmissionInput, PreparedInputMode};
use crate::client_admissions::ClientAdmissions;
use crate::effect_ledger::EffectLedger;
use crate::effect_session::{
    EffectSession, EffectSessionEvent, QueuedChangeStatus, QueuedInputView, SetModelError,
};
use futures::future::BoxFuture;
use maho_core::agent_session::{
    AgentSession, AgentSessionSubscription, QueuedInputChange, QueuedInputOptions,
};
use maho_ext_api::{AbortSource, AgentEvent, AgentSessionEvent, InputSource};
use serde_json::Value;
use std::sync::Arc;

pub struct AgentEffectSession {
    session: AgentSession,
    route: String,
}

impl AgentEffectSession {
    /// `route` keys the ledgers (senpi keys the `AgentSession` object); one per live session.
    pub fn new(session: AgentSession, route: String) -> Self {
        Self { session, route }
    }

    /// Subscribe the ledgers of `effects` to `session`'s events. Dropping the subscription stops it.
    pub fn observe(
        session: &AgentSession,
        effects: Arc<dyn EffectSession>,
    ) -> AgentSessionSubscription {
        let route = effects.effect_route();
        session.subscribe(Arc::new(move |event: &AgentSessionEvent| {
            let Some(event) = narrow(event) else {
                return;
            };
            if let Some(admissions) = ClientAdmissions::existing(&route) {
                admissions.observe(&event);
            }
            if let Some(ledger) = EffectLedger::existing(&route) {
                ledger.observe(&event);
            }
        }))
    }
}

/// The session events the ledgers react to.
pub fn narrow(event: &AgentSessionEvent) -> Option<EffectSessionEvent> {
    Some(match event {
        AgentSessionEvent::Agent(AgentEvent::MessageStart {
            message: maho_ext_api::AgentMessage::Llm(maho_ai::types::Message::User(_)),
            ..
        }) =>
        {
            EffectSessionEvent::UserMessageStart {
                client_message_id: None,
            }
        }
        AgentSessionEvent::Agent(AgentEvent::AgentStart) => EffectSessionEvent::AgentStart,
        AgentSessionEvent::AgentEnd {
            messages,
            will_retry,
            ..
        } => EffectSessionEvent::AgentEnd {
            will_retry: *will_retry,
            assistant_error: messages
                .last()
                .and_then(maho_ext_api::AgentMessage::as_assistant)
                .filter(|assistant| assistant.stop_reason == maho_ai::types::StopReason::Error)
                .map(|assistant| assistant.error_message.clone()),
        },
        AgentSessionEvent::AgentIdle => EffectSessionEvent::AgentIdle,
        AgentSessionEvent::QueueUpdate { ordered, .. } => EffectSessionEvent::QueueUpdate {
            queued_ids: ordered
                .iter()
                .map(|input| input.client_message_id.clone())
                .collect(),
        },
        AgentSessionEvent::ModelChanged { .. } => {
            EffectSessionEvent::ModelChanged { effect_id: None }
        }
        _ => return None,
    })
}

/// Core's `QueuedInputChange` as the effect seam names it.
fn queued_change_status(change: QueuedInputChange) -> QueuedChangeStatus {
    match change {
        QueuedInputChange::Removed => QueuedChangeStatus::Removed,
        QueuedInputChange::Edited => QueuedChangeStatus::Edited,
        QueuedInputChange::AlreadyDelivered => QueuedChangeStatus::AlreadyDelivered,
        QueuedInputChange::NotFound => QueuedChangeStatus::NotFound,
        QueuedInputChange::Conflict => QueuedChangeStatus::Conflict,
    }
}

fn abort_source_name(source: AbortSource) -> &'static str {
    match source {
        AbortSource::User => "user",
        AbortSource::System => "system",
        AbortSource::Provider => "provider",
    }
}

impl EffectSession for AgentEffectSession {
    fn durable_session_id(&self) -> String {
        self.session.session_id()
    }

    fn effect_route(&self) -> String {
        self.route.clone()
    }

    fn entries(&self) -> Vec<Value> {
        self.session
            .with_session_manager(|manager| manager.entries())
    }

    fn append_custom_entry(&self, custom_type: &str, data: Value) {
        let entry = self
            .session
            .with_session_manager_mut(|manager| manager.append_custom(custom_type, Some(data)));
        drop(entry);
    }

    fn persist_header_now(&self) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async move {
            self.session
                .with_session_manager_mut(|manager| manager.persist_header_now())
        })
    }

    fn session_file(&self) -> Option<String> {
        self.session.session_file()
    }

    fn is_streaming(&self) -> bool {
        self.session.is_streaming()
    }

    fn current_abort_source(&self) -> Option<String> {
        self.session
            .current_abort_source()
            .map(|source| abort_source_name(source).to_owned())
    }

    fn streaming_client_message_id(&self) -> Option<String> {
        // Core's streaming message carries no client identity.
        None
    }

    fn restore_queued_input(
        &self,
        prepared: PreparedAdmissionInput,
    ) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async move {
            let images = prepared.images.map(|images| {
                images
                    .into_iter()
                    .map(|image| maho_ai::types::ImageContent {
                        data: image.data,
                        mime_type: image.mime_type,
                    })
                    .collect()
            });
            let options = QueuedInputOptions {
                source: Some(InputSource::Rpc),
                ..Default::default()
            };
            match prepared.mode {
                PreparedInputMode::Steer => {
                    self.session.steer(&prepared.text, images, options).await
                }
                PreparedInputMode::FollowUp => {
                    self.session
                        .follow_up(&prepared.text, images, options)
                        .await
                }
            }
        })
    }

    fn clear_queue(&self, abort_will_follow: bool) -> usize {
        let cleared = self.session.clear_queue(abort_will_follow);
        cleared.steering.len() + cleared.follow_up.len()
    }

    fn queued_inputs(&self) -> Vec<QueuedInputView> {
        self.session
            .get_queued_inputs()
            .into_iter()
            .map(|input| QueuedInputView {
                queued_id: input.queued_id,
                client_message_id: input.client_message_id,
                text: input.text,
                revision: input.revision,
            })
            .collect()
    }

    fn change_queued_input(
        &self,
        queued_id: &str,
        text: Option<String>,
        expected_revision: Option<u64>,
        effect_id: Option<String>,
    ) -> BoxFuture<'_, Result<QueuedChangeStatus, String>> {
        let queued_id = queued_id.to_owned();
        Box::pin(async move {
            self.session
                .change_queued_input(&queued_id, text, expected_revision, effect_id)
                .await
                .map(queued_change_status)
        })
    }

    fn find_available_model(
        &self,
        provider: &str,
        model_id: &str,
    ) -> BoxFuture<'_, Result<Option<Value>, String>> {
        let found = self
            .session
            .model_registry()
            .get_available()
            .into_iter()
            .find(|model| model.provider == provider && model.id == model_id)
            .map(|model| serde_json::to_value(model).map_err(|error| error.to_string()))
            .transpose();
        Box::pin(async move { found })
    }

    fn set_model(
        &self,
        provider: &str,
        model_id: &str,
        _effect_id: Option<String>,
    ) -> BoxFuture<'_, Result<Option<String>, SetModelError>> {
        let model = self.session.model_registry().find(provider, model_id);
        let missing = format!("Model not found: {provider}/{model_id}");
        Box::pin(async move {
            let model = model.ok_or(SetModelError::Other(missing))?;
            self.session
                .set_model(model)
                .await
                .map(|change| change.and_then(|change| change.system_prompt_name))
                .map_err(SetModelError::Other)
        })
    }

    fn available_thinking_levels(&self) -> Vec<String> {
        self.session
            .get_available_thinking_levels()
            .into_iter()
            .map(|level| level.as_str().to_owned())
            .collect()
    }

    fn set_thinking_level(&self, level: &str, turn_scope: bool, _effect_id: Option<String>) {
        let Some(level) = self
            .session
            .get_available_thinking_levels()
            .into_iter()
            .find(|candidate| candidate.as_str() == level)
        else {
            return;
        };
        if turn_scope {
            self.session.set_session_thinking_level(level);
        } else {
            self.session.set_thinking_level(level);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn assistant(stop: &str, error: Option<&str>) -> maho_ext_api::AgentMessage {
        let message: maho_ai::types::AssistantMessage = serde_json::from_value(json!({
            "role": "assistant", "content": [], "api": "faux", "provider": "faux", "model": "faux-1",
            "usage": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
                "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0}},
            "stopReason": stop, "errorMessage": error, "timestamp": 0
        }))
        .expect("assistant message");
        maho_ext_api::AgentMessage::Llm(maho_ai::types::Message::Assistant(std::sync::Arc::new(message)))
    }

    fn agent_end(last: maho_ext_api::AgentMessage, will_retry: bool) -> AgentSessionEvent {
        AgentSessionEvent::AgentEnd {
            messages: vec![last],
            aborted: false,
            will_retry,
            abort_source: None,
        }
    }

    #[test]
    fn an_agent_end_carries_the_last_assistant_error() {
        assert_eq!(
            narrow(&agent_end(assistant("error", Some("boom")), false)),
            Some(EffectSessionEvent::AgentEnd {
                will_retry: false,
                assistant_error: Some(Some("boom".into())),
            })
        );
        assert_eq!(
            narrow(&agent_end(assistant("stop", None), true)),
            Some(EffectSessionEvent::AgentEnd {
                will_retry: true,
                assistant_error: None,
            })
        );
    }

    #[test]
    fn lifecycle_and_queue_events_are_forwarded_and_others_dropped() {
        assert_eq!(
            narrow(&AgentSessionEvent::Agent(AgentEvent::AgentStart)),
            Some(EffectSessionEvent::AgentStart)
        );
        assert_eq!(
            narrow(&AgentSessionEvent::AgentIdle),
            Some(EffectSessionEvent::AgentIdle)
        );
        assert_eq!(
            narrow(&AgentSessionEvent::QueueUpdate {
                steering: vec!["a".into()],
                follow_up: Vec::new(),
                ordered: vec![
                    maho_ext_api::QueuedInput {
                        text: "a".into(),
                        mode: maho_ext_api::StreamingBehavior::Steer,
                        enqueue_order: 1,
                        queued_id: "q_1".into(),
                        revision: 1,
                        client_message_id: None,
                    },
                    maho_ext_api::QueuedInput {
                        text: "b".into(),
                        mode: maho_ext_api::StreamingBehavior::FollowUp,
                        enqueue_order: 2,
                        queued_id: "q_2".into(),
                        revision: 1,
                        client_message_id: Some("c-2".into()),
                    },
                ],
            }),
            Some(EffectSessionEvent::QueueUpdate {
                queued_ids: vec![None, Some("c-2".into())]
            })
        );
        assert_eq!(narrow(&AgentSessionEvent::AgentSettled), None);
    }

    #[test]
    fn abort_sources_use_the_wire_names() {
        assert_eq!(abort_source_name(AbortSource::User), "user");
        assert_eq!(abort_source_name(AbortSource::System), "system");
        assert_eq!(abort_source_name(AbortSource::Provider), "provider");
    }
}
