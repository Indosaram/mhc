//! senpi `modes/rpc/structured-session-records.ts`: the session's internal structured lane behind the
//! compact-updates flag, and the seeded block gate every structured subscriber runs.
//!
//! The provider capture rides beside each event (`Captured`), the seeded projection is
//! `json_event::to_seeded_json_event`, and the lane is `AgentSession::subscribe_structured`;
//! [`subscribe_structured_session_records`] joins them through the gate below.
use std::collections::{BTreeMap, HashMap};

use serde_json::{Value, json};

/// The engine-socket compact-updates rollout flag (`<brand>_ENGINE_SOCKET_COMPACT_UPDATES`), explicit
/// `1`/`true` only. Off, no session holds a structured subscription and every destination keeps its bytes.
pub fn structured_lane_enabled() -> bool {
    let env: HashMap<String, String> = std::env::vars().collect();
    matches!(
        maho_core::brand::env_value("ENGINE_SOCKET_COMPACT_UPDATES", &env).as_deref(),
        Some("1" | "true")
    )
}

/// The capture fields of one record the gate reads (upstream `getAssistantCapture(record)`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StructuredCapture {
    pub content_index: Option<usize>,
    pub offset: Option<usize>,
    pub next_offset: Option<usize>,
    /// Whether the terminal capture carries its frozen message.
    pub has_message: bool,
    pub rewritten_from: Option<usize>,
}

/// The seeded projection failed its frontier check (upstream `SeedFrontierError`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeedFrontier;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockState {
    Open,
    Ended,
    Invalid,
}

/// Admits a block's records only after its own seed or insert reached this subscriber, so a subscriber
/// that attached mid-message gets no seed, delta or control for that message, never a wrong one. A
/// started block whose next record has no capture, or that has no accepted end when the message ends, is
/// invalid: its later records are dropped and the assistant `message_end` names it in `invalidatedBlocks`.
/// A terminal that rewrote blocks from `rewrittenFrom` on invalidates every block at or above that index,
/// and a `message_end` with no interpretable terminal capture invalidates every block of the message.
#[derive(Default)]
pub struct SeededBlockGate {
    blocks: Option<BTreeMap<usize, BlockState>>,
    next_offsets: HashMap<usize, usize>,
}

impl SeededBlockGate {
    /// `seed` is the seeded projection: `Ok(None)` when nothing was captured, `Err` at a frontier failure.
    pub fn accept(
        &mut self,
        record: &Value,
        capture: Option<&StructuredCapture>,
        seed: impl FnOnce() -> Result<Option<Value>, SeedFrontier>,
    ) -> Result<Option<Value>, SeedFrontier> {
        let kind = record
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let index_of = |record: &Value| {
            record
                .get("contentIndex")
                .and_then(Value::as_u64)
                .and_then(|n| usize::try_from(n).ok())
        };
        match kind {
            "message_block_state" | "message_block_metadata" => {
                let phase = (kind == "message_block_state")
                    .then(|| record.get("phase").and_then(Value::as_str))
                    .flatten();
                return Ok(self.control(record, index_of(record), phase));
            }
            "message_tool_arguments_reset" => {
                let accepted = self.control(record, index_of(record), None);
                if accepted.is_some()
                    && let Some(index) = index_of(record)
                {
                    let prefix = record["rawArgumentPrefix"].as_str().unwrap_or_default();
                    self.next_offsets
                        .insert(index, prefix.encode_utf16().count());
                }
                return Ok(accepted);
            }
            _ => {}
        }
        let assistant = record["message"]["role"].as_str() == Some("assistant");
        if kind == "message_start" && assistant {
            let seeded = seed()?;
            self.blocks = seeded.as_ref().map(|_| BTreeMap::new());
            self.next_offsets.clear();
            return Ok(seeded);
        }
        if kind == "message_end" && assistant {
            return Ok(Some(self.end(record, capture, seed)?));
        }
        if kind != "message_update" {
            return seed();
        }
        if self.blocks.is_none() {
            return Ok(None);
        }
        let update = &record["assistantMessageEvent"];
        let Some(public_index) = index_of(update) else {
            return seed();
        };
        let index = capture
            .and_then(|capture| capture.content_index)
            .unwrap_or(public_index);
        let update_type = update["type"].as_str().unwrap_or_default();
        let starts = matches!(
            update_type,
            "text_start" | "thinking_start" | "toolcall_start"
        );
        let blocks = self.blocks.get_or_insert_with(BTreeMap::new);
        if starts && self.next_offsets.contains_key(&index) {
            blocks.insert(index, BlockState::Invalid);
            return Ok(None);
        }
        let state = blocks.get(&index).copied();
        if state == Some(BlockState::Invalid) || (!starts && state != Some(BlockState::Open)) {
            return Ok(None);
        }
        let seeded = match seed() {
            Ok(seeded) => seeded,
            Err(SeedFrontier) => {
                blocks.insert(index, BlockState::Invalid);
                return Ok(None);
            }
        };
        if starts
            && seeded.is_some()
            && let Some(next) = capture.and_then(|capture| capture.next_offset)
        {
            self.next_offsets.insert(index, next);
        }
        if let Some(delta) = update.get("delta").and_then(Value::as_str)
            && seeded.is_some()
        {
            let offset = capture.and_then(|capture| capture.offset);
            if offset.is_none() || offset != self.next_offsets.get(&index).copied() {
                blocks.insert(index, BlockState::Invalid);
                return Ok(None);
            }
            self.next_offsets
                .insert(index, offset.unwrap_or(0) + delta.encode_utf16().count());
        }
        let ends = matches!(update_type, "text_end" | "thinking_end" | "toolcall_end");
        blocks.insert(
            index,
            match (&seeded, ends) {
                (None, _) => BlockState::Invalid,
                (Some(_), true) => BlockState::Ended,
                (Some(_), false) => BlockState::Open,
            },
        );
        Ok(seeded)
    }

    fn control(
        &mut self,
        record: &Value,
        index: Option<usize>,
        phase: Option<&str>,
    ) -> Option<Value> {
        let blocks = self.blocks.as_mut()?;
        let index = index?;
        let state = blocks.get(&index).copied();
        if state == Some(BlockState::Invalid) {
            return None;
        }
        match phase {
            Some("insert") => {
                blocks.insert(index, BlockState::Open);
            }
            _ if state.is_none() || (phase == Some("end") && state != Some(BlockState::Open)) => {
                return None;
            }
            Some("end") => {
                blocks.insert(index, BlockState::Ended);
            }
            _ => {}
        }
        Some(record.clone())
    }

    fn end(
        &mut self,
        record: &Value,
        capture: Option<&StructuredCapture>,
        seed: impl FnOnce() -> Result<Option<Value>, SeedFrontier>,
    ) -> Result<Value, SeedFrontier> {
        let rewritten_from = match capture {
            Some(capture) if capture.has_message => capture.rewritten_from.unwrap_or(usize::MAX),
            _ => 0,
        };
        let invalidated: Vec<usize> = self
            .blocks
            .take()
            .unwrap_or_default()
            .into_iter()
            .filter(|(index, state)| !(*state == BlockState::Ended && *index < rewritten_from))
            .map(|(index, _)| index)
            .collect();
        self.next_offsets.clear();
        let mut seeded = seed()?.unwrap_or_else(|| record.clone());
        if !invalidated.is_empty()
            && let Some(object) = seeded.as_object_mut()
        {
            object.insert("invalidatedBlocks".into(), json!(invalidated));
        }
        Ok(seeded)
    }
}

impl StructuredCapture {
    /// The gate's view of a provider capture (`getAssistantCapture(record)`).
    pub fn of(capture: &maho_ai::utils::assistant_capture::AssistantCapture) -> Self {
        Self {
            content_index: capture.content_index,
            offset: capture.offset,
            next_offset: capture.next_offset,
            has_message: capture.message.is_some(),
            rewritten_from: capture.rewritten_from,
        }
    }
}

/// senpi `subscribeStructuredSessionRecords`: the session's internal structured lane, projected like its legacy
/// records: seeded `toJsonEvent` projections of every session event plus the three block controls, in session order,
/// admitted through one [`SeededBlockGate`]. `output` never reaches a legacy destination. `Err` when the session's
/// agent has no internal channel. Dropping the subscription detaches it.
pub fn subscribe_structured_session_records(
    session: &maho_core::agent_session::AgentSession,
    output: std::sync::Arc<dyn Fn(Value) + Send + Sync>,
) -> Result<
    maho_core::structured_session_lane::StructuredSubscription,
    maho_core::structured_session_lane::StructuredLaneUnavailableError,
> {
    subscribe_structured_session_records_routed(session, output, None, None)
}

pub fn subscribe_structured_session_records_routed(
    session: &maho_core::agent_session::AgentSession,
    output: std::sync::Arc<dyn Fn(Value) + Send + Sync>,
    route: Option<&str>,
    last_abort_source: Option<crate::connection_handler::LastAbortSource>,
) -> Result<
    maho_core::structured_session_lane::StructuredSubscription,
    maho_core::structured_session_lane::StructuredLaneUnavailableError,
> {
    use maho_core::structured_session_lane::AgentSessionStructuredRecord as Record;
    let gate = std::sync::Mutex::new(SeededBlockGate::default());
    let route_str = route
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| session.session_id());
    let last_abort_source = last_abort_source.unwrap_or_default();
    let weak_session = session.weak_accessor();
    let carrier_session = session.weak_accessor();
    let correlated = std::sync::Mutex::new(crate::session_binding::new_client_message_events(
        output,
        route_str.clone(),
    ));
    session.subscribe_structured(std::sync::Arc::new(move |record: &Record| {
        let (value, capture) = match record {
            Record::Progress(progress) => match serde_json::to_value(&**progress) {
                Ok(value) => (value, None),
                Err(_) => return,
            },
            Record::Event(event) => {
                let projected = match weak_session() {
                    Some(session) => crate::connection_handler::rpc_session_event_record_routed(
                        &session,
                        &event.value,
                        &last_abort_source,
                        Some(&route_str),
                    ),
                    None => crate::session_binding::session_event_record(&event.value),
                };
                match projected {
                    Ok(value) => (value, event.capture.as_deref()),
                    Err(_) => return,
                }
            }
        };
        let view = capture.map(StructuredCapture::of);
        let accepted = gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .accept(&value, view.as_ref(), || {
                crate::json_event::to_seeded_json_event(&value, capture).map_err(|_| SeedFrontier)
            });
        if let Ok(Some(accepted)) = accepted {
            let carrier = carrier_session().and_then(|session| session.recovery_carrier(&accepted));
            correlated
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .accept(accepted, carrier);
        }
    }))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn update(kind: &str, index: usize, delta: Option<&str>) -> Value {
        let mut event = json!({"type":kind,"contentIndex":index});
        if let Some(delta) = delta {
            event["delta"] = json!(delta);
        }
        json!({"type":"message_update","message":{"role":"assistant"},"assistantMessageEvent":event})
    }

    fn cap(offset: Option<usize>, next: Option<usize>) -> StructuredCapture {
        StructuredCapture {
            offset,
            next_offset: next,
            ..StructuredCapture::default()
        }
    }

    fn pass(record: &Value) -> impl FnOnce() -> Result<Option<Value>, SeedFrontier> + '_ {
        move || Ok(Some(record.clone()))
    }

    // senpi 4272-structured-lane-validity: offset gaps invalidate a block and the terminal names it.
    #[test]
    fn invalidates_gapped_blocks_and_names_them_at_the_end() {
        let mut gate = SeededBlockGate::default();
        let start = json!({"type":"message_start","message":{"role":"assistant"}});
        assert!(
            gate.accept(&start, None, pass(&start))
                .expect("start")
                .is_some()
        );
        for (index, records) in [
            (
                0,
                vec![
                    (update("text_start", 0, None), cap(None, Some(0))),
                    (update("text_delta", 0, Some("ab")), cap(Some(0), None)),
                    (update("text_end", 0, None), cap(None, None)),
                ],
            ),
            (
                1,
                vec![
                    (update("text_start", 1, None), cap(None, Some(0))),
                    (update("text_delta", 1, Some("x")), cap(Some(5), None)),
                ],
            ),
        ] {
            for (record, capture) in records {
                let _ = gate
                    .accept(&record, Some(&capture), pass(&record))
                    .expect("update");
            }
            let _ = index;
        }
        let late = update("text_delta", 1, Some("y"));
        assert!(
            gate.accept(&late, Some(&cap(Some(1), None)), pass(&late))
                .expect("late")
                .is_none()
        );
        let end = json!({"type":"message_end","message":{"role":"assistant"}});
        let terminal = StructuredCapture {
            has_message: true,
            ..StructuredCapture::default()
        };
        let out = gate
            .accept(&end, Some(&terminal), pass(&end))
            .expect("end")
            .expect("record");
        assert_eq!(out["invalidatedBlocks"], json!([1]));
    }

    #[test]
    fn a_mid_message_subscriber_gets_nothing_for_that_message() {
        let mut gate = SeededBlockGate::default();
        let delta = update("text_delta", 0, Some("a"));
        assert!(
            gate.accept(&delta, Some(&cap(Some(0), None)), pass(&delta))
                .expect("delta")
                .is_none()
        );
        let control = json!({"type":"message_block_state","contentIndex":0,"phase":"end"});
        assert!(
            gate.accept(&control, None, pass(&control))
                .expect("control")
                .is_none()
        );
        let end = json!({"type":"message_end","message":{"role":"assistant"}});
        let out = gate
            .accept(&end, None, pass(&end))
            .expect("end")
            .expect("record");
        assert!(out.get("invalidatedBlocks").is_none());
    }

    #[test]
    fn a_terminal_without_capture_invalidates_every_block() {
        let mut gate = SeededBlockGate::default();
        let start = json!({"type":"message_start","message":{"role":"assistant"}});
        let _ = gate.accept(&start, None, pass(&start));
        let first = update("text_start", 2, None);
        let _ = gate.accept(&first, Some(&cap(None, Some(0))), pass(&first));
        let end_block = update("text_end", 2, None);
        let _ = gate.accept(&end_block, Some(&cap(None, None)), pass(&end_block));
        let end = json!({"type":"message_end","message":{"role":"assistant"}});
        let out = gate
            .accept(&end, None, || Ok(None))
            .expect("end")
            .expect("record");
        assert_eq!(out["invalidatedBlocks"], json!([2]));
    }

    /// senpi 4272-structured-lane-bridge: the structured lane reaches its subscriber as seeded projections with the
    /// block control between the assistant's start and end; a session with no internal channel refuses it.
    #[tokio::test]
    async fn a_live_session_streams_seeded_records_and_controls_in_order() {
        use maho_ai::utils::assistant_capture::{
            AssistantProgress, BlockPhase, push_assistant_progress,
        };
        use std::sync::{Arc, Mutex};
        let channel = maho_agent::create_internal_agent_channel();
        let stream_fn: maho_agent::types::StreamFn = Arc::new(|_, _, _| {
            let stream =
                maho_ai::utils::event_stream::AssistantMessageEventStream::with_capture_blocks(
                    true,
                );
            let mut partial =
                maho_ai::providers::faux::faux_assistant_message("", Default::default());
            partial.content.clear();
            stream.push(maho_ai::types::AssistantMessageEvent::Start {
                partial: partial.clone(),
            });
            let native = maho_ai::types::ContentBlock::ProviderNative(
                maho_ai::types::ProviderNativeContent {
                    subtype: "fixture".into(),
                    raw: json!({"value": 1}),
                },
            );
            partial.content.push(native.clone());
            push_assistant_progress(
                &stream,
                AssistantProgress::block_state(0, BlockPhase::Insert, &native),
            );
            stream.push(maho_ai::types::AssistantMessageEvent::Done {
                reason: maho_ai::types::DoneReason::Stop,
                message: partial.clone(),
            });
            stream.end(Some(partial));
            stream
        });
        let config = |agent| maho_core::agent_session::AgentSessionConfig {
            agent,
            session_manager: maho_core::session_manager::SessionManager::in_memory(
                "/tmp", None, None,
            ),
            settings_manager: maho_core::settings_manager::SettingsManager::from_storage(
                Box::new(maho_core::settings_manager::InMemorySettingsStorage::default()),
                false,
            ),
            cwd: "/tmp".to_owned(),
            agent_dir: Some("/tmp/maho-agent".to_owned()),
            fallback_now: None,
            retry_random: None,
            scoped_models: Vec::new(),
            favorite_models: Vec::new(),
            flag_values: Default::default(),
            custom_tools: Vec::new(),
            model_runtime: Some(
                maho_core::model_runtime::ModelRuntime::create_sync(
                    maho_core::model_runtime::CreateModelRuntimeOptions {
                        providers: Some(Vec::new()),
                        ..Default::default()
                    },
                )
                .expect("valid model runtime"),
            ),
            model_registry: None,
            uses_default_stream_function: Some(false),
            initial_active_tool_names: None,
            default_tool_names: None,
            eval_only_tool_names: None,
            allowed_tool_names: None,
            excluded_tool_names: None,
            base_tools_override: None,
            session_start_event: None,
            auto_title_sessions: None,
        };
        let plain = maho_core::agent_session::AgentSession::new(config(maho_agent::Agent::new(
            maho_agent::AgentOptions {
                stream_fn: Some(stream_fn.clone()),
                ..Default::default()
            },
        )))
        .expect("session");
        assert!(subscribe_structured_session_records(&plain, Arc::new(|_| {})).is_err());
        let session = maho_core::agent_session::AgentSession::new_with_internal_channel(
            config(maho_agent::Agent::new(maho_agent::AgentOptions {
                internal_channel: Some(channel.clone()),
                stream_fn: Some(stream_fn),
                ..Default::default()
            })),
            None,
            Some(channel),
        )
        .expect("session");
        let records = Arc::new(Mutex::new(Vec::<Value>::new()));
        let seen = records.clone();
        let subscription = subscribe_structured_session_records(
            &session,
            Arc::new(move |record| seen.lock().unwrap().push(record)),
        )
        .expect("the session layer owns the channel");
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            session
                .agent()
                .prompt(maho_agent::AgentPromptInput::text("go")),
        )
        .await
        .expect("bounded turn");
        drop(subscription);
        let records = records.lock().unwrap().clone();
        let kinds: Vec<&str> = records
            .iter()
            .map(|record| record["type"].as_str().unwrap_or_default())
            .collect();
        let start = records
            .iter()
            .rposition(|record| {
                record["type"] == "message_start" && record["message"]["role"] == "assistant"
            })
            .expect("assistant message_start");
        let control = kinds
            .iter()
            .position(|kind| *kind == "message_block_state")
            .expect("the block control passed the gate");
        let end = records
            .iter()
            .rposition(|record| {
                record["type"] == "message_end" && record["message"]["role"] == "assistant"
            })
            .expect("assistant message_end");
        assert!(start < control && control < end, "{kinds:?}");
        assert_eq!(records[control]["phase"], "insert");
    }

    pub(crate) fn session_of(
        agent: maho_agent::Agent,
        channel: Option<maho_agent::InternalAgentChannel>,
        root: &std::path::Path,
    ) -> maho_core::agent_session::AgentSession {
        maho_core::agent_session::AgentSession::new_with_internal_channel(
            maho_core::agent_session::AgentSessionConfig {
                agent,
                session_manager: maho_core::session_manager::SessionManager::in_memory(
                    &root.to_string_lossy(),
                    None,
                    None,
                ),
                settings_manager: maho_core::settings_manager::SettingsManager::from_storage(
                    Box::new(maho_core::settings_manager::InMemorySettingsStorage::default()),
                    false,
                ),
                cwd: root.to_string_lossy().into_owned(),
                agent_dir: Some(root.join("agent").to_string_lossy().into_owned()),
                fallback_now: None,
                retry_random: None,
                scoped_models: Vec::new(),
                favorite_models: Vec::new(),
                flag_values: Default::default(),
                custom_tools: Vec::new(),
                model_runtime: Some(
                    maho_core::model_runtime::ModelRuntime::create_sync(
                        maho_core::model_runtime::CreateModelRuntimeOptions {
                            providers: Some(Vec::new()),
                            agent_dir: Some(root.join("agent")),
                            models_path: Some(root.join("models.json")),
                            auth_path: Some(root.join("auth.json")),
                            allow_model_network: Some(false),
                            refresh_on_create: Some(false),
                            ..Default::default()
                        },
                    )
                    .expect("valid model runtime"),
                ),
                model_registry: None,
                uses_default_stream_function: Some(false),
                initial_active_tool_names: None,
                default_tool_names: None,
                eval_only_tool_names: None,
                allowed_tool_names: None,
                excluded_tool_names: None,
                base_tools_override: None,
                session_start_event: None,
                auto_title_sessions: Some(false),
            },
            None,
            channel,
        )
        .expect("session")
    }

    pub(crate) fn scripted_session(
        root: &std::path::Path,
        script: impl Fn(&maho_ai::types::AssistantMessageEventStream, maho_ai::types::AssistantMessage)
        + Send
        + Sync
        + 'static,
    ) -> maho_core::agent_session::AgentSession {
        let channel = maho_agent::create_internal_agent_channel();
        let stream_fn: maho_agent::types::StreamFn = std::sync::Arc::new(move |_, _, _| {
            let stream = maho_ai::types::AssistantMessageEventStream::with_capture_blocks(true);
            let mut partial =
                maho_ai::providers::faux::faux_assistant_message("", Default::default());
            partial.content.clear();
            script(&stream, partial);
            stream
        });
        let agent = maho_agent::Agent::new(maho_agent::AgentOptions {
            internal_channel: Some(channel.clone()),
            stream_fn: Some(stream_fn),
            ..Default::default()
        });
        session_of(agent, Some(channel), root)
    }

    fn text(text: &str) -> maho_ai::types::ContentBlock {
        maho_ai::types::ContentBlock::Text(maho_ai::types::TextContent {
            text: text.into(),
            ..Default::default()
        })
    }

    async fn lane_turn(session: &maho_core::agent_session::AgentSession) -> Vec<Value> {
        let records = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = records.clone();
        let subscription = subscribe_structured_session_records(
            session,
            std::sync::Arc::new(move |record: Value| sink.lock().expect("records").push(record)),
        )
        .expect("the session layer owns the channel");
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            session
                .agent()
                .prompt(maho_agent::AgentPromptInput::text("go")),
        )
        .await
        .expect("bounded turn");
        drop(subscription);
        let records = records.lock().expect("records").clone();
        first_assistant_message(&records)
    }

    fn first_assistant_message(records: &[Value]) -> Vec<Value> {
        let Some(start) = records
            .iter()
            .position(|r| r["type"] == "message_start" && r["message"]["role"] == "assistant")
        else {
            return Vec::new();
        };
        let Some(end) = records[start..]
            .iter()
            .position(|r| r["type"] == "message_end")
        else {
            return Vec::new();
        };
        records[start..=start + end].to_vec()
    }

    fn labels(records: &[Value]) -> Vec<String> {
        records
            .iter()
            .filter_map(|record| {
                let kind = record["type"].as_str()?;
                if !kind.starts_with("message_") {
                    return None;
                }
                if let Some(event) = record.get("assistantMessageEvent") {
                    return Some(format!("{kind}:{}", event["type"].as_str().unwrap_or("")));
                }
                Some(match record.get("phase").and_then(Value::as_str) {
                    Some(phase) => format!("{kind}:{phase}"),
                    None => kind.to_owned(),
                })
            })
            .collect()
    }

    #[tokio::test]
    async fn invalidates_every_block_at_or_above_a_rewriting_terminals_rewritten_from() {
        use maho_ai::types::{AssistantMessageEvent as E, ContentBlock, DoneReason, ToolCall};
        let root = tempfile::tempdir().unwrap();
        let session = scripted_session(
            root.path(),
            |stream: &maho_ai::types::AssistantMessageEventStream,
             mut partial: maho_ai::types::AssistantMessage| {
                stream.push(E::Start {
                    partial: partial.clone(),
                });
                partial.content.push(text("a"));
                stream.push(E::TextStart {
                    content_index: 0,
                    partial: partial.clone(),
                });
                stream.push(E::TextEnd {
                    content_index: 0,
                    content: "a".into(),
                    partial: partial.clone(),
                });
                let mut arguments = serde_json::Map::new();
                arguments.insert("a".into(), json!(1));
                let call = ToolCall {
                    id: "call_1".into(),
                    name: "probe".into(),
                    arguments,
                    ..Default::default()
                };
                partial.content.push(ContentBlock::ToolCall(call.clone()));
                stream.push(E::ToolcallStart {
                    content_index: 1,
                    partial: partial.clone(),
                });
                stream.push(E::ToolcallEnd {
                    content_index: 1,
                    tool_call: call,
                    partial: partial.clone(),
                });
                partial.content.push(text("b"));
                stream.push(E::TextStart {
                    content_index: 2,
                    partial: partial.clone(),
                });
                stream.push(E::TextEnd {
                    content_index: 2,
                    content: "b".into(),
                    partial: partial.clone(),
                });
                partial
                    .content
                    .retain(|block| !matches!(block, ContentBlock::ToolCall(_)));
                stream.push_with_hint(
                    E::Done {
                        reason: DoneReason::Stop,
                        message: partial.clone(),
                    },
                    maho_ai::utils::assistant_capture::CaptureHint::rewritten_from(Some(1)),
                );
                stream.end(Some(partial));
            },
        );
        let turn = lane_turn(&session).await;
        assert_eq!(
            labels(&turn),
            [
                "message_start",
                "message_update:text_start",
                "message_update:text_end",
                "message_update:toolcall_start",
                "message_update:toolcall_end",
                "message_update:text_start",
                "message_update:text_end",
                "message_end",
            ]
        );
        let end = turn.last().expect("message_end");
        assert_eq!(end["invalidatedBlocks"], json!([1, 2]));
        assert_eq!(
            end["message"]["content"],
            json!([{"type":"text","text":"a"},{"type":"text","text":"b"}])
        );
    }

    #[tokio::test]
    async fn invalidates_every_block_of_a_message_that_ends_without_a_terminal_capture() {
        use maho_ai::types::AssistantMessageEvent as E;
        let root = tempfile::tempdir().unwrap();
        let session = scripted_session(
            root.path(),
            |stream: &maho_ai::types::AssistantMessageEventStream,
             mut partial: maho_ai::types::AssistantMessage| {
                stream.push(E::Start {
                    partial: partial.clone(),
                });
                for value in ["a", "b"] {
                    let content_index = partial.content.len();
                    partial.content.push(text(value));
                    stream.push(E::TextStart {
                        content_index,
                        partial: partial.clone(),
                    });
                    stream.push(E::TextEnd {
                        content_index,
                        content: value.into(),
                        partial: partial.clone(),
                    });
                }
                stream.end(Some(partial));
            },
        );
        let turn = lane_turn(&session).await;
        assert_eq!(
            labels(&turn),
            [
                "message_start",
                "message_update:text_start",
                "message_update:text_end",
                "message_update:text_start",
                "message_update:text_end",
                "message_end",
            ]
        );
        assert_eq!(
            turn.last().expect("message_end")["invalidatedBlocks"],
            json!([0, 1])
        );
    }

    #[tokio::test]
    async fn orders_block_controls_among_seeded_records() {
        use maho_ai::types::{
            AssistantMessageEvent as E, ContentBlock, DoneReason, ProviderNativeContent,
        };
        use maho_ai::utils::assistant_capture::{
            AssistantProgress, BlockPhase, push_assistant_progress,
        };
        let root = tempfile::tempdir().unwrap();
        let session = scripted_session(
            root.path(),
            |stream: &maho_ai::types::AssistantMessageEventStream,
             mut partial: maho_ai::types::AssistantMessage| {
                stream.push(E::Start {
                    partial: partial.clone(),
                });
                partial.content.push(text(""));
                stream.push(E::TextStart {
                    content_index: 0,
                    partial: partial.clone(),
                });
                partial.content[0] = text("hi");
                stream.push(E::TextDelta {
                    content_index: 0,
                    delta: "hi".into(),
                    partial: partial.clone(),
                });
                stream.push(E::TextEnd {
                    content_index: 0,
                    content: "hi".into(),
                    partial: partial.clone(),
                });
                let native = ContentBlock::ProviderNative(ProviderNativeContent {
                    subtype: "fixture".into(),
                    raw: json!({"value":1}),
                });
                partial.content.push(native.clone());
                push_assistant_progress(
                    stream,
                    AssistantProgress::block_state(1, BlockPhase::Insert, &native),
                );
                push_assistant_progress(
                    stream,
                    AssistantProgress::block_state(1, BlockPhase::End, &native),
                );
                stream.push(E::Done {
                    reason: DoneReason::Stop,
                    message: partial.clone(),
                });
                stream.end(Some(partial));
            },
        );
        let turn = lane_turn(&session).await;
        assert_eq!(
            labels(&turn),
            [
                "message_start",
                "message_update:text_start",
                "message_update:text_delta",
                "message_update:text_end",
                "message_block_state:insert",
                "message_block_state:end",
                "message_end",
            ]
        );
        let update = |kind: &str| {
            turn.iter()
                .find(|record| record["assistantMessageEvent"]["type"] == kind)
                .expect("seeded update")
        };
        assert_eq!(
            update("text_start")["assistantMessageEvent"]["seed"]["coveredCodeUnits"],
            0
        );
        assert_eq!(update("text_delta")["assistantMessageEvent"]["offset"], 0);
        assert!(
            turn.last()
                .expect("message_end")
                .get("invalidatedBlocks")
                .is_none()
        );
        for key in ["messageId", "turnId", "runtimeItemKey"] {
            assert!(turn.iter().all(|record| record.get(key).is_none()), "{key}");
        }
    }

    #[tokio::test]
    async fn gives_a_lane_attached_mid_message_no_seed_delta_or_control() {
        use maho_ai::types::{
            AssistantMessageEvent as E, ContentBlock, DoneReason, ProviderNativeContent,
        };
        use maho_ai::utils::assistant_capture::{
            AssistantProgress, BlockMetadata, BlockPhase, push_assistant_progress,
        };
        use std::sync::atomic::{AtomicUsize, Ordering};

        let (resume_tx, resume_rx) = tokio::sync::watch::channel(false);
        let turn_index = std::sync::Arc::new(AtomicUsize::new(0));
        let turn_index_clone = turn_index.clone();

        let channel = maho_agent::create_internal_agent_channel();
        let stream_fn: maho_agent::types::StreamFn = std::sync::Arc::new(move |_, _, _| {
            let stream = maho_ai::types::AssistantMessageEventStream::with_capture_blocks(true);
            let mut partial =
                maho_ai::providers::faux::faux_assistant_message("", Default::default());
            partial.content.clear();
            let turn = turn_index_clone.fetch_add(1, Ordering::SeqCst);
            if turn == 0 {
                let stream_clone = stream.clone();
                let mut rx = resume_rx.clone();
                tokio::spawn(async move {
                    stream_clone.push(E::Start {
                        partial: partial.clone(),
                    });
                    partial.content.push(text("a"));
                    stream_clone.push(E::TextStart {
                        content_index: 0,
                        partial: partial.clone(),
                    });
                    let _ = rx.wait_for(|&v| v).await;
                    partial.content[0] = text("ab");
                    stream_clone.push(E::TextDelta {
                        content_index: 0,
                        delta: "b".into(),
                        partial: partial.clone(),
                    });
                    push_assistant_progress(
                        &stream_clone,
                        AssistantProgress::metadata(BlockMetadata {
                            content_index: 0,
                            text_signature: Some("sig".into()),
                            ..Default::default()
                        }),
                    );
                    let native = ContentBlock::ProviderNative(ProviderNativeContent {
                        subtype: "fixture".into(),
                        raw: json!({}),
                    });
                    partial.content.push(native.clone());
                    push_assistant_progress(
                        &stream_clone,
                        AssistantProgress::block_state(1, BlockPhase::Insert, &native),
                    );
                    push_assistant_progress(
                        &stream_clone,
                        AssistantProgress::block_state(1, BlockPhase::End, &native),
                    );
                    stream_clone.push(E::TextEnd {
                        content_index: 0,
                        content: "ab".into(),
                        partial: partial.clone(),
                    });
                    stream_clone.push(E::Done {
                        reason: DoneReason::Stop,
                        message: partial.clone(),
                    });
                    stream_clone.end(Some(partial));
                });
            } else {
                stream.push(E::Start {
                    partial: partial.clone(),
                });
                partial.content.push(text("c"));
                stream.push(E::TextStart {
                    content_index: 0,
                    partial: partial.clone(),
                });
                partial.content[0] = text("cd");
                stream.push(E::TextDelta {
                    content_index: 0,
                    delta: "d".into(),
                    partial: partial.clone(),
                });
                stream.push(E::TextEnd {
                    content_index: 0,
                    content: "cd".into(),
                    partial: partial.clone(),
                });
                stream.push(E::Done {
                    reason: DoneReason::Stop,
                    message: partial.clone(),
                });
                stream.end(Some(partial));
            }
            stream
        });
        let agent = maho_agent::Agent::new(maho_agent::AgentOptions {
            internal_channel: Some(channel.clone()),
            stream_fn: Some(stream_fn),
            ..Default::default()
        });
        let root = tempfile::tempdir().unwrap();
        let session = session_of(agent, Some(channel), root.path());

        let (emitted_tx, emitted_rx) = tokio::sync::oneshot::channel::<()>();
        let emitted_tx = std::sync::Arc::new(std::sync::Mutex::new(Some(emitted_tx)));
        let watch_subscription =
            session.subscribe(std::sync::Arc::new(move |event| {
                if let maho_ext_api::AgentSessionEvent::Agent(
                    maho_agent::types::AgentEvent::MessageUpdate {
                        assistant_message_event:
                            maho_ai::types::AssistantMessageEvent::TextStart { .. },
                        ..
                    },
                ) = event
                    && let Some(tx) = emitted_tx.lock().unwrap().take()
                {
                    let _ = tx.send(());
                }
            }));

        let early_records = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let early_sink = early_records.clone();
        let early_sub = subscribe_structured_session_records(
            &session,
            std::sync::Arc::new(move |record| early_sink.lock().unwrap().push(record)),
        )
        .expect("early subscription");

        let late_records = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let late_sink = late_records.clone();

        let prompt_session = session.clone();
        let prompt_task = tokio::spawn(async move {
            prompt_session
                .agent()
                .prompt(maho_agent::AgentPromptInput::text("first"))
                .await
        });

        tokio::time::timeout(std::time::Duration::from_secs(5), emitted_rx)
            .await
            .expect("text_start emitted")
            .expect("emitted rx");

        let late_sub = subscribe_structured_session_records(
            &session,
            std::sync::Arc::new(move |record| late_sink.lock().unwrap().push(record)),
        )
        .expect("late subscription");

        resume_tx.send(true).expect("resume signal");

        tokio::time::timeout(std::time::Duration::from_secs(5), prompt_task)
            .await
            .expect("first turn bounded")
            .expect("first turn join");

        let early = early_records.lock().unwrap().clone();
        assert_eq!(
            labels(&first_assistant_message(&early)),
            [
                "message_start",
                "message_update:text_start",
                "message_update:text_delta",
                "message_block_metadata",
                "message_block_state:insert",
                "message_block_state:end",
                "message_update:text_end",
                "message_end",
            ]
        );

        let late = late_records.lock().unwrap().clone();
        let late_message_labels: Vec<String> = late
            .iter()
            .filter_map(|r| {
                let kind = r["type"].as_str()?;
                kind.starts_with("message_").then(|| kind.to_string())
            })
            .collect();
        assert_eq!(late_message_labels, ["message_end"]);
        assert!(
            late.last()
                .expect("message_end")
                .get("invalidatedBlocks")
                .is_none()
        );

        late_records.lock().unwrap().clear();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            session
                .agent()
                .prompt(maho_agent::AgentPromptInput::text("second")),
        )
        .await
        .expect("second turn bounded");

        let late2 = late_records.lock().unwrap().clone();
        assert_eq!(
            labels(&first_assistant_message(&late2)),
            [
                "message_start",
                "message_update:text_start",
                "message_update:text_delta",
                "message_update:text_end",
                "message_end",
            ]
        );

        drop(late_sub);
        drop(early_sub);
        drop(watch_subscription);
    }
}
