//! Port of `claim-receipt/index.ts`: the claim-receipt component and its session state.
//!
//! Shadow mode by default: verdicts are recorded and logged, but nothing reaches the agent until the
//! `omo-senpi-claim-receipt-nudge` flag is on. The upstream JS sidecar import (`runtime-loader.ts`)
//! is JS-only bundling: this component calls [`crate::runtime::evaluate_answer`] directly.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, LazyLock, Mutex};

use maho_ext_api::*;
use regex::Regex;
use serde_json::{json, Value};

use maho_omo_start_work_continuation::agent_end_eligibility::read_agent_end_outcome;

use crate::r#match::ClaimVerdict;
use crate::runtime::evaluate_answer;
use crate::verdict::{
    gate_flags_block, nudge_text, VerdictRecord, GATE_FLAGS_TAG, NUDGE_MESSAGE_TYPE,
    VERDICT_ENTRY_TYPE,
};

pub const CLAIM_RECEIPT_NUDGE_FLAG: &str = "omo-senpi-claim-receipt-nudge";

const GATE_REVIEWER_SOURCE: &str = r"(?:^|-)gate-reviewer$";
const MEMORY_CHILD_SENTINELS: [&str; 2] = ["SENPI_MEMORY_REFLECTION", "SENPI_MEMORY_FACTS"];
const MAX_GATE_FLAGS: usize = 20;

static GATE_REVIEWER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(GATE_REVIEWER_SOURCE).expect("valid gate reviewer regex"));

#[derive(Clone, Debug, Default)]
struct SessionState {
    epoch: u64,
    nudged_epoch: i64,
    agent_end: Option<Value>,
    flagged: Vec<ClaimVerdict>,
}

type Sessions = Arc<Mutex<HashMap<String, SessionState>>>;

fn session_key(ctx: &ExtensionContext) -> String {
    let id = ctx.session_manager.session_id();
    if id.is_empty() {
        "anonymous".to_owned()
    } else {
        id.to_owned()
    }
}

fn with_state<T>(sessions: &Sessions, key: &str, act: impl FnOnce(&mut SessionState) -> T) -> T {
    let mut guard = sessions
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let state = guard.entry(key.to_owned()).or_insert_with(|| SessionState {
        epoch: 0,
        nudged_epoch: -1,
        agent_end: None,
        flagged: Vec::new(),
    });
    act(state)
}

/// The branch as the JSON entries upstream reads: `data` plus the entry's own id and type.
fn branch_entries(ctx: &ExtensionContext) -> Vec<Value> {
    ctx.session_manager
        .get_branch()
        .into_iter()
        .map(|entry| {
            let mut value = entry.data;
            if let Some(object) = value.as_object_mut() {
                object.insert("id".to_owned(), Value::String(entry.id));
                object.insert("type".to_owned(), Value::String(entry.kind));
            }
            value
        })
        .collect()
}

fn last_assistant_index(branch: &[Value]) -> Option<usize> {
    branch.iter().enumerate().rev().find_map(|(index, entry)| {
        let is_answer = entry["type"].as_str() == Some("message")
            && entry["message"].is_object()
            && entry["message"]["role"].as_str() == Some("assistant");
        is_answer.then_some(index)
    })
}

/// The claim-receipt component. `env` carries the memory-worker sentinels that exclude child runs.
#[derive(Clone, Debug)]
pub struct ClaimReceiptComponent {
    pub env: BTreeMap<String, String>,
}

impl ClaimReceiptComponent {
    #[must_use]
    pub fn new(env: BTreeMap<String, String>) -> Self {
        Self { env }
    }

    #[must_use]
    pub fn from_process_env() -> Self {
        Self::new(std::env::vars().collect())
    }

    fn memory_child(&self) -> bool {
        MEMORY_CHILD_SENTINELS
            .iter()
            .any(|name| self.env.get(*name).map(String::as_str) == Some("1"))
    }
}

impl Default for ClaimReceiptComponent {
    fn default() -> Self {
        Self::from_process_env()
    }
}

impl Extension for ClaimReceiptComponent {
    fn register(&self, api: &mut ExtensionApi) {
        api.register_flag(
            CLAIM_RECEIPT_NUDGE_FLAG,
            FlagType::Boolean {
                default: Some(false),
            },
            Some(
                "Show the unsupported-claim nudge and pass flagged claims to gate reviewers. Off by default: verdicts are only recorded."
                    .to_owned(),
            ),
        );
        let runtime = api.runtime.clone();
        let surfaced: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(move || {
            runtime.get_flag(CLAIM_RECEIPT_NUDGE_FLAG) == Some(FlagValue::Boolean(true))
        });
        let sessions: Sessions = Arc::new(Mutex::new(HashMap::new()));
        let memory_child = self.memory_child();

        let input_sessions = Arc::clone(&sessions);
        api.on(
            EventKind::Input,
            Arc::new(move |event, ctx| {
                let sessions = Arc::clone(&input_sessions);
                Box::pin(async move {
                    let ExtensionEvent::Input(input) = event else {
                        return Ok(EventResult::None);
                    };
                    if input.source == InputSource::Extension {
                        return Ok(EventResult::Input(InputEventResult::Continue));
                    }
                    let key = session_key(ctx);
                    with_state(&sessions, &key, |state| {
                        state.epoch += 1;
                        state.flagged = Vec::new();
                    });
                    Ok(EventResult::Input(InputEventResult::Continue))
                })
            }),
        );

        let agent_end_sessions = Arc::clone(&sessions);
        api.on(
            EventKind::AgentEnd,
            Arc::new(move |event, ctx| {
                let sessions = Arc::clone(&agent_end_sessions);
                Box::pin(async move {
                    let ExtensionEvent::AgentEnd {
                        messages,
                        aborted,
                        will_retry,
                        ..
                    } = event
                    else {
                        return Ok(EventResult::None);
                    };
                    let payload = json!({
                        "aborted": aborted.unwrap_or(false),
                        "willRetry": will_retry.unwrap_or(false),
                        "messages": serde_json::to_value(messages).unwrap_or(Value::Null),
                    });
                    let key = session_key(ctx);
                    with_state(&sessions, &key, |state| state.agent_end = Some(payload));
                    Ok(EventResult::None)
                })
            }),
        );

        let settle_sessions = Arc::clone(&sessions);
        let settle_surfaced = Arc::clone(&surfaced);
        api.on(
            EventKind::AgentBeforeSettle,
            Arc::new(move |event, ctx| {
                let sessions = Arc::clone(&settle_sessions);
                let surfaced = Arc::clone(&settle_surfaced);
                Box::pin(async move {
                    let ExtensionEvent::AgentBeforeSettle {
                        entries, context, ..
                    } = event
                    else {
                        return Ok(EventResult::None);
                    };
                    // The native event carries the payload's `outcome` inside `context`; an absent
                    // outcome is the ordinary completed settle.
                    if context
                        .get("outcome")
                        .and_then(Value::as_str)
                        .is_some_and(|outcome| outcome != "completed")
                    {
                        return Ok(EventResult::None);
                    }
                    if memory_child {
                        return Ok(EventResult::None);
                    }
                    let key = session_key(ctx);
                    let Some(ended) = with_state(&sessions, &key, |state| state.agent_end.take())
                    else {
                        return Ok(EventResult::None);
                    };
                    if read_agent_end_outcome(&ended).blocked_by.is_some() {
                        return Ok(EventResult::None);
                    }
                    let branch = branch_entries(ctx);
                    let Some(answer_index) = last_assistant_index(&branch) else {
                        return Ok(EventResult::None);
                    };
                    let answer_entry = &branch[answer_index];
                    if !answer_entry["message"].is_object() {
                        return Ok(EventResult::None);
                    }
                    let content = answer_entry["message"]["content"].clone();
                    if content.as_array().is_some_and(|parts| {
                        parts
                            .iter()
                            .any(|part| part["type"].as_str() == Some("toolCall"))
                    }) {
                        return Ok(EventResult::None);
                    }
                    // Evaluation is synchronous here, so no user message can arrive mid-flight; the
                    // epoch guard below keeps a stale result from contaminating a newer epoch anyway.
                    let epoch = with_state(&sessions, &key, |state| state.epoch);
                    let Some(evaluated) = evaluate_answer(&branch, answer_index as f64, &content)
                    else {
                        return Ok(EventResult::None);
                    };
                    let flagged = evaluated.flagged;
                    let surfaced_now = surfaced();
                    let nudged = with_state(&sessions, &key, |state| {
                        let current = state.epoch == epoch;
                        let nudged = current
                            && !flagged.is_empty()
                            && state.nudged_epoch != i64::try_from(epoch).unwrap_or(i64::MAX)
                            && surfaced_now;
                        if nudged {
                            state.nudged_epoch = i64::try_from(epoch).unwrap_or(i64::MAX);
                        }
                        if current {
                            state.flagged.extend(flagged.iter().cloned());
                            let excess = state.flagged.len().saturating_sub(MAX_GATE_FLAGS);
                            if excess > 0 {
                                state.flagged.drain(0..excess);
                            }
                        }
                        nudged
                    });
                    let answer_entry_id = answer_entry["id"].as_str().unwrap_or("").to_owned();
                    let record = VerdictRecord {
                        version: VerdictRecord::VERSION,
                        answer_entry_id: answer_entry_id.clone(),
                        epoch,
                        flagged: !flagged.is_empty(),
                        nudged,
                        verdicts: evaluated.verdicts,
                    };
                    entries.push(json!({
                        "type": "custom",
                        "customType": VERDICT_ENTRY_TYPE,
                        "data": record.to_value(),
                    }));
                    if nudged {
                        entries.push(json!({
                            "type": "custom_message",
                            "customType": NUDGE_MESSAGE_TYPE,
                            "content": nudge_text(&flagged),
                            "display": true,
                            "details": {
                                "answerEntryId": answer_entry_id,
                                "flagged": flagged
                                    .iter()
                                    .map(crate::verdict::verdict_value)
                                    .collect::<Vec<_>>(),
                            },
                        }));
                    }
                    if !flagged.is_empty()
                        && let Some(logger) = ctx.logger.as_ref()
                    {
                        logger.info(
                            "omo-senpi claim-receipt flagged unsupported claims",
                            Some(&json!({ "count": flagged.len(), "nudged": nudged })),
                        );
                    }
                    Ok(EventResult::None)
                })
            }),
        );

        let tool_sessions = Arc::clone(&sessions);
        let tool_surfaced = Arc::clone(&surfaced);
        api.on(
            EventKind::ToolCall,
            Arc::new(move |event, ctx| {
                let sessions = Arc::clone(&tool_sessions);
                let surfaced = Arc::clone(&tool_surfaced);
                Box::pin(async move {
                    if !surfaced() {
                        return Ok(EventResult::None);
                    }
                    let ExtensionEvent::ToolCall(call) = event else {
                        return Ok(EventResult::None);
                    };
                    if call.tool_name != "task" || !call.input.is_object() {
                        return Ok(EventResult::None);
                    }
                    let key = session_key(ctx);
                    let flagged = with_state(&sessions, &key, |state| state.flagged.clone());
                    if flagged.is_empty() {
                        return Ok(EventResult::None);
                    }
                    let block = gate_flags_block(&flagged);
                    let tagged = format!("<{GATE_FLAGS_TAG}>");
                    let parent_agent = call.input["subagent_type"].as_str().map(str::to_owned);
                    let task_count = call.input["tasks"].as_array().map_or(0, Vec::len);
                    for slot in 0..=task_count {
                        let target = if slot == 0 {
                            &mut call.input
                        } else {
                            &mut call.input["tasks"][slot - 1]
                        };
                        if !target.is_object() {
                            continue;
                        }
                        let agent = target["subagent_type"]
                            .as_str()
                            .map(str::to_owned)
                            .or_else(|| parent_agent.clone());
                        let Some(agent) = agent else {
                            continue;
                        };
                        if !GATE_REVIEWER.is_match(&agent) {
                            continue;
                        }
                        let Some(prompt) = target["prompt"].as_str().map(str::to_owned) else {
                            continue;
                        };
                        if prompt.contains(&tagged) {
                            continue;
                        }
                        target["prompt"] = Value::String(format!("{prompt}\n\n{block}"));
                    }
                    Ok(EventResult::None)
                })
            }),
        );

        let compact_sessions = Arc::clone(&sessions);
        api.on(
            EventKind::SessionCompact,
            Arc::new(move |event, ctx| {
                let sessions = Arc::clone(&compact_sessions);
                Box::pin(async move {
                    if matches!(
                        event,
                        ExtensionEvent::SessionCompact(SessionCompactEvent::Rejected { .. })
                    ) {
                        return Ok(EventResult::None);
                    }
                    let key = session_key(ctx);
                    with_state(&sessions, &key, |state| state.nudged_epoch = -1);
                    Ok(EventResult::None)
                })
            }),
        );

        let shutdown_sessions = Arc::clone(&sessions);
        api.on(
            EventKind::SessionShutdown,
            Arc::new(move |_event, ctx| {
                let sessions = Arc::clone(&shutdown_sessions);
                Box::pin(async move {
                    let key = session_key(ctx);
                    sessions
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .remove(&key);
                    Ok(EventResult::None)
                })
            }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_gate_reviewer_agent_name_matches() {
        assert!(GATE_REVIEWER.is_match("omo-native-gate-reviewer"));
        assert!(GATE_REVIEWER.is_match("gate-reviewer"));
        assert!(!GATE_REVIEWER.is_match("omo-native-gate-reviewers"));
        assert!(!GATE_REVIEWER.is_match("explore"));
    }

    #[test]
    fn the_last_assistant_message_is_the_answer() {
        let branch = json!([
            { "type": "message", "id": "u", "message": { "role": "user", "content": [] } },
            { "type": "message", "id": "a", "message": { "role": "assistant", "content": [] } },
            { "type": "message", "id": "t", "message": { "role": "toolResult", "content": [] } }
        ]);
        let branch = branch.as_array().cloned().unwrap_or_default();
        assert_eq!(last_assistant_index(&branch), Some(1));
        assert_eq!(last_assistant_index(&[]), None);
    }
}
