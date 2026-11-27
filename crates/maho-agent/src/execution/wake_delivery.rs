//! Port of senpi `packages/agent/src/execution/wake-delivery.ts`.

use std::collections::BTreeSet;
use std::sync::Arc;

use maho_ai::types::{ContentBlock, ImageContent, TextContent};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::wake_inbox::WakeCheckFn;
use super::wake_link::WakeDrop;
use super::wake_types::{WAKE_DROPPED_SOURCE, WakeEvent, WakeRefusal};

pub const ONE_SHOT_MODES: [&str; 2] = ["print", "json"];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WakeContent {
    Text(String),
    Parts(Vec<ContentBlock>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HandWakeMessage {
    pub custom_type: String,
    pub content: WakeContent,
    pub display: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HandWakeDeliverAs {
    Steer,
    FollowUp,
    NextTurn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HandWakeMessageOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_turn: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deliver_as: Option<HandWakeDeliverAs>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HandWakeUserDeliverAs {
    Steer,
    FollowUp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HandWakeUserMessageOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deliver_as: Option<HandWakeUserDeliverAs>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expand_prompt_templates: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HandWakeNoticeType {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq)]
pub enum HandWakeDelivery {
    Message {
        session_id: Option<String>,
        message: HandWakeMessage,
        options: Option<HandWakeMessageOptions>,
    },
    UserMessage {
        session_id: Option<String>,
        content: WakeContent,
        options: Option<HandWakeUserMessageOptions>,
    },
    Entry {
        session_id: Option<String>,
        custom_type: String,
        data: Option<Value>,
    },
    Notice {
        session_id: Option<String>,
        message: String,
        notice_type: Option<HandWakeNoticeType>,
    },
    Dropped {
        reason: String,
        count: u64,
    },
}

impl HandWakeDelivery {
    pub fn session_id(&self) -> Option<&str> {
        match self {
            Self::Message { session_id, .. }
            | Self::UserMessage { session_id, .. }
            | Self::Entry { session_id, .. }
            | Self::Notice { session_id, .. } => session_id.as_deref(),
            Self::Dropped { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("malformed hand wake {event_id}: {what}")]
pub struct MalformedHandWakeError {
    pub event_id: String,
    pub what: String,
}

fn fail(event_id: &str, what: &str) -> MalformedHandWakeError {
    MalformedHandWakeError {
        event_id: event_id.to_owned(),
        what: what.to_owned(),
    }
}

fn parse_content(value: Option<&Value>) -> Option<WakeContent> {
    let val = value?;
    if let Some(s) = val.as_str() {
        return Some(WakeContent::Text(s.to_owned()));
    }
    let arr = val.as_array()?;
    let mut parts = Vec::with_capacity(arr.len());
    for item in arr {
        let obj = item.as_object()?;
        match obj.get("type")?.as_str()? {
            "text" => {
                let text = obj.get("text")?.as_str()?.to_owned();
                parts.push(ContentBlock::Text(TextContent {
                    text,
                    audience: None,
                    text_signature: None,
                }));
            }
            "image" => {
                let data = obj.get("data")?.as_str()?.to_owned();
                let mime_type = obj.get("mimeType")?.as_str()?.to_owned();
                parts.push(ContentBlock::Image(ImageContent { data, mime_type }));
            }
            _ => return None,
        }
    }
    Some(WakeContent::Parts(parts))
}

fn parse_message(value: Option<&Value>) -> Option<HandWakeMessage> {
    let obj = value?.as_object()?;
    let custom_type = obj.get("customType")?.as_str()?.to_owned();
    let display = obj.get("display")?.as_bool()?;
    let content = parse_content(obj.get("content"))?;
    let details = obj.get("details").cloned();
    Some(HandWakeMessage {
        custom_type,
        content,
        display,
        details,
    })
}

pub fn parse_hand_wake(wake: &WakeEvent) -> Result<HandWakeDelivery, MalformedHandWakeError> {
    let Some(payload) = wake.payload.as_object() else {
        return Err(fail(&wake.event_id, "payload"));
    };
    let session_id = match payload.get("sessionId") {
        None => None,
        Some(v) => Some(
            v.as_str()
                .ok_or_else(|| fail(&wake.event_id, "sessionId"))?
                .to_owned(),
        ),
    };
    match wake.source.as_str() {
        "message" => {
            let options = match payload.get("options") {
                None => None,
                Some(val) => {
                    let obj = val
                        .as_object()
                        .ok_or_else(|| fail(&wake.event_id, "message options"))?;
                    let trigger_turn = match obj.get("triggerTurn") {
                        None => None,
                        Some(v) => Some(
                            v.as_bool()
                                .ok_or_else(|| fail(&wake.event_id, "message options"))?,
                        ),
                    };
                    let deliver_as = match obj.get("deliverAs") {
                        None => None,
                        Some(v) => {
                            let d = match v.as_str() {
                                Some("steer") => HandWakeDeliverAs::Steer,
                                Some("followUp") => HandWakeDeliverAs::FollowUp,
                                Some("nextTurn") => HandWakeDeliverAs::NextTurn,
                                _ => return Err(fail(&wake.event_id, "message options")),
                            };
                            Some(d)
                        }
                    };
                    Some(HandWakeMessageOptions {
                        trigger_turn,
                        deliver_as,
                    })
                }
            };
            let message = parse_message(payload.get("message"))
                .ok_or_else(|| fail(&wake.event_id, "message"))?;
            Ok(HandWakeDelivery::Message {
                session_id,
                message,
                options,
            })
        }
        "user_message" => {
            let options = match payload.get("options") {
                None => None,
                Some(val) => {
                    let obj = val
                        .as_object()
                        .ok_or_else(|| fail(&wake.event_id, "user message options"))?;
                    let deliver_as = match obj.get("deliverAs") {
                        None => None,
                        Some(v) => {
                            let d = match v.as_str() {
                                Some("steer") => HandWakeUserDeliverAs::Steer,
                                Some("followUp") => HandWakeUserDeliverAs::FollowUp,
                                _ => return Err(fail(&wake.event_id, "user message options")),
                            };
                            Some(d)
                        }
                    };
                    let expand_prompt_templates = match obj.get("expandPromptTemplates") {
                        None => None,
                        Some(v) => Some(
                            v.as_bool()
                                .ok_or_else(|| fail(&wake.event_id, "user message options"))?,
                        ),
                    };
                    Some(HandWakeUserMessageOptions {
                        deliver_as,
                        expand_prompt_templates,
                    })
                }
            };
            let content = parse_content(payload.get("content"))
                .ok_or_else(|| fail(&wake.event_id, "user message content"))?;
            Ok(HandWakeDelivery::UserMessage {
                session_id,
                content,
                options,
            })
        }
        "entry" => {
            let custom_type = payload
                .get("customType")
                .and_then(Value::as_str)
                .ok_or_else(|| fail(&wake.event_id, "entry customType"))?
                .to_owned();
            let data = payload.get("data").cloned();
            Ok(HandWakeDelivery::Entry {
                session_id,
                custom_type,
                data,
            })
        }
        "notice" => {
            let message = payload
                .get("message")
                .and_then(Value::as_str)
                .ok_or_else(|| fail(&wake.event_id, "notice"))?
                .to_owned();
            let notice_type = match payload.get("type") {
                None => None,
                Some(v) => Some(match v.as_str() {
                    Some("info") => HandWakeNoticeType::Info,
                    Some("warning") => HandWakeNoticeType::Warning,
                    Some("error") => HandWakeNoticeType::Error,
                    _ => return Err(fail(&wake.event_id, "notice type")),
                }),
            };
            Ok(HandWakeDelivery::Notice {
                session_id,
                message,
                notice_type,
            })
        }
        WAKE_DROPPED_SOURCE => {
            let reason = payload
                .get("reason")
                .and_then(Value::as_str)
                .ok_or_else(|| fail(&wake.event_id, "drop report"))?
                .to_owned();
            let count = payload
                .get("count")
                .and_then(Value::as_u64)
                .ok_or_else(|| fail(&wake.event_id, "drop report"))?;
            Ok(HandWakeDelivery::Dropped { reason, count })
        }
        other => Err(fail(&wake.event_id, &format!("source {other}"))),
    }
}

#[derive(Debug, Clone)]
pub struct HandWakeSessionContext {
    pub mode: String,
    pub has_model: bool,
}

pub trait HandWakeSession: Send + Sync {
    fn send_message(&self, message: HandWakeMessage, options: Option<HandWakeMessageOptions>);
    fn send_user_message(&self, content: WakeContent, options: Option<HandWakeUserMessageOptions>);
    fn append_entry(&self, custom_type: String, data: Option<Value>);
    fn notify(&self, message: String, notice_type: Option<HandWakeNoticeType>);
    fn context(&self) -> Option<HandWakeSessionContext>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandWakeOutcome {
    Delivered,
    Skipped,
}

fn may_start_turn(session: &dyn HandWakeSession) -> bool {
    session
        .context()
        .is_some_and(|ctx| !ONE_SHOT_MODES.contains(&ctx.mode.as_str()) && ctx.has_model)
}

fn drop_notice(count: u64, reason: &str) -> String {
    let plural = if count == 1 { "" } else { "s" };
    format!("{count} late result{plural} from work on the user's machine never arrived ({reason}).")
}

pub fn deliver_hand_wake(
    wake: &WakeEvent,
    session: &dyn HandWakeSession,
) -> Result<HandWakeOutcome, MalformedHandWakeError> {
    match parse_hand_wake(wake)? {
        HandWakeDelivery::Message {
            message, options, ..
        } => {
            if options.and_then(|o| o.trigger_turn) == Some(true) && !may_start_turn(session) {
                return Ok(HandWakeOutcome::Skipped);
            }
            session.send_message(message, options);
            Ok(HandWakeOutcome::Delivered)
        }
        HandWakeDelivery::UserMessage {
            content, options, ..
        } => {
            if !may_start_turn(session) {
                return Ok(HandWakeOutcome::Skipped);
            }
            session.send_user_message(content, options);
            Ok(HandWakeOutcome::Delivered)
        }
        HandWakeDelivery::Entry {
            custom_type, data, ..
        } => {
            session.append_entry(custom_type, data);
            Ok(HandWakeOutcome::Delivered)
        }
        HandWakeDelivery::Notice {
            message,
            notice_type,
            ..
        } => {
            session.notify(message, notice_type);
            Ok(HandWakeOutcome::Delivered)
        }
        HandWakeDelivery::Dropped { reason, count } => {
            session.notify(
                drop_notice(count, &reason),
                Some(HandWakeNoticeType::Warning),
            );
            Ok(HandWakeOutcome::Delivered)
        }
    }
}

pub fn deliver_hand_drop(drop: &WakeDrop, session: &dyn HandWakeSession) {
    session.notify(
        drop_notice(drop.count, drop.reason.as_str()),
        Some(HandWakeNoticeType::Warning),
    );
}

pub type SessionsOfFn = Arc<dyn Fn(&str) -> Option<BTreeSet<String>> + Send + Sync>;

pub fn scope_session_check(sessions_of: SessionsOfFn) -> WakeCheckFn {
    Arc::new(move |wake| {
        let session_id_val = wake.payload.as_object()?.get("sessionId")?;
        let Some(sessions) = sessions_of(&wake.scope) else {
            return Some(WakeRefusal::UnknownScope);
        };
        match session_id_val.as_str() {
            Some(id) if sessions.contains(id) => None,
            _ => Some(WakeRefusal::NotScopeSession),
        }
    })
}
