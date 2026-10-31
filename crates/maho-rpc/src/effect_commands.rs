//! The effect-bearing RPC commands (senpi `rpc/effect-commands.ts`): `abort`, `set_model`,
//! `set_thinking_level`, `dequeue_message`, `edit_queued_message` and `get_effect_status`. With an `effectId`
//! each one is a ledger effect, written `accepted` before it is applied and answered with its status; a
//! same-id retry replays the stored answer and performs nothing.
use crate::abort_grace::{ABORT_DEFAULT_GRACE_MS, ABORT_MAX_GRACE_MS, is_abort_grace_in_range};
use crate::client_admissions::ClientAdmissions;
use crate::effect_ledger::{EffectBegin, EffectLedger, EffectLedgerError, EffectPatch};
use crate::effect_outcome_copy::{
    context_budget_outcome, effect_outcomes, model_switch_error_outcome,
    provider_unavailable_outcome, unknown_model_outcome, unsupported_thinking_level_outcome,
};
use crate::effect_record::{
    EffectActor, EffectCommandKind, EffectIdConflict, EffectOutcomeKind, EffectState, EffectStatus,
    QueuedChangeAnswer,
};
use crate::effect_session::{EffectSession, QueuedChangeStatus, SetModelError};
use crate::effect_transcript_lookup::{
    EffectLookup, TranscriptUnreadable, lookup_effect_in_transcript,
};
use crate::rpc_types::{
    RPC_ERROR_CONTEXT_BUDGET, RPC_ERROR_EFFECT_ID_CONFLICT, RPC_ERROR_INVALID_ARGUMENT,
    RPC_ERROR_MODEL_NOT_FOUND, RPC_ERROR_PROVIDER_UNAVAILABLE, RPC_ERROR_TRANSCRIPT_UNREADABLE,
};
use crate::session_control_actions_data::unsupported_thinking_level;
use futures::future::BoxFuture;
use serde_json::{Map, Value, json};
use std::sync::Arc;
use std::time::Duration;

pub struct EffectCommandContext {
    pub session: Arc<dyn EffectSession>,
    pub by: EffectActor,
    /// The routing handle this session is answered on, absent on a classic connection.
    pub session_id: Option<String>,
}

fn respond(id: &Value, command: &str, data: Option<Value>) -> Value {
    let mut response = Map::new();
    if !id.is_null() {
        response.insert("id".into(), id.clone());
    }
    response.insert("type".into(), "response".into());
    response.insert("command".into(), command.into());
    response.insert("success".into(), true.into());
    if let Some(data) = data {
        response.insert("data".into(), data);
    }
    Value::Object(response)
}

fn refuse(
    id: &Value,
    command: &str,
    message: &str,
    error_code: Option<&str>,
    error_data: Option<Value>,
) -> Value {
    let mut response = Map::new();
    if !id.is_null() {
        response.insert("id".into(), id.clone());
    }
    response.insert("type".into(), "response".into());
    response.insert("command".into(), command.into());
    response.insert("success".into(), false.into());
    response.insert("error".into(), message.into());
    if let Some(code) = error_code {
        response.insert("errorCode".into(), code.into());
    }
    if let Some(data) = error_data {
        response.insert("errorData".into(), data);
    }
    Value::Object(response)
}

fn object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

/// senpi `effectAnswer`.
pub fn effect_answer(status: &EffectStatus, session_id: Option<&str>, replayed: bool) -> Value {
    let mut effect = status.clone();
    if session_id.is_some() {
        effect.session_id = session_id.map(str::to_owned);
    }
    let mut answer = json!({
        "effectId": status.effect_id,
        "state": status.state,
        "effect": effect,
    });
    if replayed {
        answer["replayed"] = true.into();
    }
    answer
}

/// senpi `beginEffect`: `Ok(begin)`, or `Err(response)` for an `effect_id_conflict`.
pub async fn begin_effect(
    ctx: &EffectCommandContext,
    kind: EffectCommandKind,
    command: &Value,
    effect_id: &str,
) -> Result<Result<EffectBegin, Value>, EffectLedgerError> {
    let ledger = EffectLedger::for_session(&ctx.session).await;
    match ledger.begin(kind, effect_id, command, ctx.by.clone()).await {
        Ok(begin) => Ok(Ok(begin)),
        Err(EffectLedgerError::Conflict(conflict)) => {
            Ok(Err(conflict_response(command, &conflict)))
        }
        Err(error) => Err(error),
    }
}

fn conflict_response(command: &Value, conflict: &EffectIdConflict) -> Value {
    refuse(
        &command["id"],
        command["type"].as_str().unwrap_or_default(),
        &conflict.to_string(),
        Some(RPC_ERROR_EFFECT_ID_CONFLICT),
        Some(json!({
            "effectId": conflict.effect_id,
            "reason": EffectIdConflict::REASON,
            "action": EffectIdConflict::ACTION,
        })),
    )
}

/// senpi `isEffectBegin`.
pub fn is_effect_begin(value: &Result<EffectBegin, Value>) -> bool {
    value.is_ok()
}

fn invalid_grace(command: &Value) -> Option<Value> {
    let grace = command.get("graceMs").filter(|grace| !grace.is_null())?;
    if is_abort_grace_in_range(grace) {
        return None;
    }
    Some(refuse(
        &command["id"],
        "abort",
        &format!("graceMs must be an integer from 0 to {ABORT_MAX_GRACE_MS}"),
        Some(RPC_ERROR_INVALID_ARGUMENT),
        Some(json!({
            "reason": "invalid_grace_ms",
            "action": format!("send graceMs as a whole number of milliseconds from 0 to {ABORT_MAX_GRACE_MS}"),
        })),
    ))
}

/// Begin the effect when the command carries an id. `Err(response)` is the answer to send as is.
async fn begin_if_effect(
    ctx: &EffectCommandContext,
    kind: EffectCommandKind,
    command: &Value,
) -> Result<Option<EffectBegin>, Value> {
    let Some(effect_id) = command["effectId"].as_str() else {
        return Ok(None);
    };
    match begin_effect(ctx, kind, command, effect_id).await {
        Ok(Ok(begin)) => Ok(Some(begin)),
        Ok(Err(response)) => Err(response),
        Err(error) => Err(refuse(
            &command["id"],
            command["type"].as_str().unwrap_or_default(),
            &error.to_string(),
            None,
            None,
        )),
    }
}

fn transitioned(
    ledger: Option<&Arc<EffectLedger>>,
    effect_id: Option<&str>,
    state: EffectState,
    patch: EffectPatch,
) -> Option<EffectStatus> {
    ledger?.transition(effect_id?, state, patch).ok()
}

/// `abort`: with `clearQueue` the queued inputs are drained (each gets a `cleared` receipt) before the
/// abort; with `awaitIdle` the answer waits until the session is idle or `graceMs` passes. `dispatch` runs
/// the abort and resolves once it settled; `idle` resolves on the next time the session is idle.
pub async fn run_abort(
    ctx: &EffectCommandContext,
    command: &Value,
    dispatch: BoxFuture<'static, ()>,
) -> Value {
    if let Some(invalid) = invalid_grace(command) {
        return invalid;
    }
    let id = &command["id"];
    let effect_id = command["effectId"].as_str();
    let await_idle = command["awaitIdle"] == true;
    let clear_queue = command["clearQueue"] == true;
    let quiesced = await_idle || clear_queue;
    match begin_if_effect(ctx, EffectCommandKind::Abort, command).await {
        Err(response) => return response,
        Ok(Some(EffectBegin::Replay(status))) => {
            return respond(
                id,
                "abort",
                Some(effect_answer(&status, ctx.session_id.as_deref(), true)),
            );
        }
        Ok(_) => {}
    }
    let admissions = ClientAdmissions::for_session(&ctx.session).await;
    let streaming = ctx.session.is_streaming();
    let mut cleared = 0;
    if clear_queue {
        admissions.clear_queued();
        cleared = ctx.session.clear_queue(true);
    }
    let noted = admissions.note_abort(ctx.by.clone(), effect_id.map(str::to_owned));
    // No turn to name: once this abort settled, forget it, unless a newer abort has been recorded since.
    let forgetting = admissions.clone();
    // Dispatched before the ledger moves, like senpi's `dispatch()`; it runs on past any grace window.
    let mut aborting = tokio::spawn(async move {
        dispatch.await;
        if !streaming {
            forgetting.forget_abort(&noted);
        }
    });
    let ledger = match effect_id {
        Some(_) => Some(EffectLedger::for_session(&ctx.session).await),
        None => None,
    };
    let mut status = if streaming {
        transitioned(
            ledger.as_ref(),
            effect_id,
            EffectState::Applied,
            EffectPatch::default(),
        )
    } else {
        transitioned(
            ledger.as_ref(),
            effect_id,
            EffectState::Done,
            EffectPatch {
                disposition: Some("no_active_turn".into()),
                ..Default::default()
            },
        )
    };
    if !quiesced {
        return respond(
            id,
            "abort",
            status
                .as_ref()
                .map(|status| effect_answer(status, ctx.session_id.as_deref(), false)),
        );
    }
    let idle = if await_idle {
        let grace = command["graceMs"]
            .as_f64()
            .map_or(ABORT_DEFAULT_GRACE_MS, |grace| grace as u64);
        let settled = tokio::time::timeout(Duration::from_millis(grace), &mut aborting).await;
        matches!(settled, Ok(Ok(()))) && !ctx.session.is_streaming()
    } else {
        !streaming
    };
    // The ledger moves an applied abort to `done` on `agent_idle`; read it again after the wait.
    if let (Some(ledger), Some(effect_id)) = (&ledger, effect_id) {
        status = ledger.status(effect_id, None).or(status);
    }
    let mut data = status
        .as_ref()
        .map(|status| object(effect_answer(status, ctx.session_id.as_deref(), false)))
        .unwrap_or_default();
    data.insert("stopped".into(), idle.into());
    data.insert("idle".into(), idle.into());
    data.insert("cleared".into(), cleared.into());
    respond(id, "abort", Some(Value::Object(data)))
}

fn replayed_status(status: &EffectStatus, command: &str) -> &'static str {
    match status.outcome.as_ref().map(|outcome| outcome.kind) {
        Some(EffectOutcomeKind::AlreadyDelivered) => "already_delivered",
        Some(EffectOutcomeKind::NotFound) => "not_found",
        Some(EffectOutcomeKind::Conflict) => "conflict",
        _ if command == "dequeue_message" => "removed",
        _ => "edited",
    }
}

fn answer_fields(data: &mut Map<String, Value>, answer: Option<&QueuedChangeAnswer>) {
    if let Some(answer) = answer {
        data.extend(object(serde_json::to_value(answer).unwrap_or(Value::Null)));
    }
}

/// `dequeue_message` / `edit_queued_message`: one queued input, by `queuedId`.
pub async fn run_queued_input_change(ctx: &EffectCommandContext, command: &Value) -> Value {
    let id = &command["id"];
    let name = command["type"].as_str().unwrap_or_default();
    let dequeue = name == "dequeue_message";
    let kind = if dequeue {
        EffectCommandKind::Dequeue
    } else {
        EffectCommandKind::EditQueued
    };
    let effect_id = command["effectId"].as_str();
    let queued_id = command["queuedId"].as_str().unwrap_or_default();
    match begin_if_effect(ctx, kind, command).await {
        Err(response) => return response,
        Ok(Some(EffectBegin::Replay(replay))) => {
            let mut data = Map::new();
            data.insert("queuedId".into(), queued_id.into());
            data.insert("status".into(), replayed_status(&replay, name).into());
            answer_fields(&mut data, replay.answer.as_ref());
            if let Some(outcome) = &replay.outcome {
                data.insert("reason".into(), json!(outcome.reason));
                data.insert("action".into(), json!(outcome.action));
            }
            data.extend(object(effect_answer(
                &replay,
                ctx.session_id.as_deref(),
                true,
            )));
            return respond(id, name, Some(Value::Object(data)));
        }
        Ok(_) => {}
    }
    let client_message_id = ctx
        .session
        .queued_inputs()
        .into_iter()
        .find(|input| input.queued_id == queued_id)
        .and_then(|input| input.client_message_id);
    let text = (!dequeue).then(|| command["text"].as_str().unwrap_or_default().to_owned());
    let status = match ctx
        .session
        .change_queued_input(
            queued_id,
            text.clone(),
            command["expectedRevision"].as_u64(),
            effect_id.map(str::to_owned),
        )
        .await
    {
        Ok(status) => status,
        Err(error) => return refuse(id, name, &error, None, None),
    };
    // Read right after the change: the revision the answer reports is the one it made or met.
    let current = ctx
        .session
        .queued_inputs()
        .into_iter()
        .find(|input| input.queued_id == queued_id);
    // A durable input is restored from its admission record on reopen: that record must carry the edit.
    if status == QueuedChangeStatus::Edited
        && let (Some(client_message_id), Some(text)) = (&client_message_id, &text)
    {
        ClientAdmissions::for_session(&ctx.session)
            .await
            .edit_queued_text(client_message_id, text);
    }
    let outcome = match status {
        QueuedChangeStatus::AlreadyDelivered => {
            Some(effect_outcomes::QUEUED_INPUT_ALREADY_DELIVERED.outcome())
        }
        QueuedChangeStatus::NotFound => Some(effect_outcomes::QUEUED_INPUT_NOT_FOUND.outcome()),
        QueuedChangeStatus::Conflict => Some(effect_outcomes::QUEUED_INPUT_CONFLICT.outcome()),
        _ => None,
    };
    let answer = match (status, &current) {
        (QueuedChangeStatus::Edited, Some(current)) => Some(QueuedChangeAnswer {
            text: Some(current.text.clone()),
            revision: Some(current.revision),
            current_revision: None,
        }),
        (QueuedChangeStatus::Conflict, Some(current)) => Some(QueuedChangeAnswer {
            text: None,
            revision: None,
            current_revision: Some(current.revision),
        }),
        _ => None,
    };
    // The receipt keeps the answer too: a same-id retry must answer exactly as this call did.
    let ledger = match effect_id {
        Some(_) => Some(EffectLedger::for_session(&ctx.session).await),
        None => None,
    };
    let recorded = match &outcome {
        None => transitioned(
            ledger.as_ref(),
            effect_id,
            EffectState::Done,
            EffectPatch {
                disposition: Some(status.as_str().into()),
                outcome: None,
                answer: answer.clone(),
            },
        ),
        Some(outcome) => transitioned(
            ledger.as_ref(),
            effect_id,
            EffectState::Failed,
            EffectPatch {
                disposition: None,
                outcome: Some(outcome.clone()),
                answer: answer.clone(),
            },
        ),
    };
    let mut data = Map::new();
    data.insert("queuedId".into(), queued_id.into());
    data.insert("status".into(), status.as_str().into());
    answer_fields(&mut data, answer.as_ref());
    if let Some(outcome) = &outcome {
        data.insert("reason".into(), json!(outcome.reason));
        data.insert("action".into(), json!(outcome.action));
    }
    if let Some(recorded) = &recorded {
        data.extend(object(effect_answer(
            recorded,
            ctx.session_id.as_deref(),
            false,
        )));
    }
    respond(id, name, Some(Value::Object(data)))
}

pub async fn run_set_model(ctx: &EffectCommandContext, command: &Value) -> Value {
    let id = &command["id"];
    let effect_id = command["effectId"].as_str();
    let provider = command["provider"].as_str().unwrap_or_default();
    let model_id = command["modelId"].as_str().unwrap_or_default();
    match begin_if_effect(ctx, EffectCommandKind::SetModel, command).await {
        Err(response) => return response,
        Ok(Some(EffectBegin::Replay(status))) => {
            return respond(
                id,
                "set_model",
                Some(effect_answer(&status, ctx.session_id.as_deref(), true)),
            );
        }
        Ok(_) => {}
    }
    let ledger = match effect_id {
        Some(_) => Some(EffectLedger::for_session(&ctx.session).await),
        None => None,
    };
    let model = match ctx.session.find_available_model(provider, model_id).await {
        Ok(model) => model,
        Err(error) => return refuse(id, "set_model", &error, None, None),
    };
    let Some(model) = model else {
        let failed = transitioned(
            ledger.as_ref(),
            effect_id,
            EffectState::Failed,
            EffectPatch {
                outcome: Some(unknown_model_outcome(provider, model_id)),
                ..Default::default()
            },
        );
        return refuse(
            id,
            "set_model",
            &format!("Model not found: {provider}/{model_id}"),
            Some(RPC_ERROR_MODEL_NOT_FOUND),
            failed.map(|failed| effect_answer(&failed, ctx.session_id.as_deref(), false)),
        );
    };
    let (Some(ledger), Some(effect_id)) = (ledger, effect_id) else {
        return match ctx.session.set_model(provider, model_id, None).await {
            Ok(system_prompt_name) => {
                let mut data = object(model);
                data.insert("systemPromptName".into(), json!(system_prompt_name));
                respond(id, "set_model", Some(Value::Object(data)))
            }
            Err(
                SetModelError::ContextBudget(message)
                | SetModelError::ProviderUnavailable(message)
                | SetModelError::Other(message),
            ) => refuse(id, "set_model", &message, None, None),
        };
    };
    let system_prompt_name = match ctx
        .session
        .set_model(provider, model_id, Some(effect_id.to_owned()))
        .await
    {
        Ok(name) => name,
        Err(SetModelError::Other(message)) => {
            let _recorded = ledger.transition(
                effect_id,
                EffectState::Failed,
                EffectPatch {
                    outcome: Some(model_switch_error_outcome(&message)),
                    ..Default::default()
                },
            );
            return refuse(id, "set_model", &message, None, None);
        }
        Err(error) => {
            let (outcome, code, message) = match error {
                SetModelError::ContextBudget(message) => (
                    context_budget_outcome(&message),
                    RPC_ERROR_CONTEXT_BUDGET,
                    message,
                ),
                SetModelError::ProviderUnavailable(message) => (
                    provider_unavailable_outcome(&message),
                    RPC_ERROR_PROVIDER_UNAVAILABLE,
                    message,
                ),
                SetModelError::Other(_) => unreachable!("handled above"),
            };
            let failed = ledger
                .transition(
                    effect_id,
                    EffectState::Failed,
                    EffectPatch {
                        outcome: Some(outcome),
                        ..Default::default()
                    },
                )
                .ok();
            return refuse(
                id,
                "set_model",
                &message,
                Some(code),
                failed.map(|failed| effect_answer(&failed, ctx.session_id.as_deref(), false)),
            );
        }
    };
    // `done` was written by the ledger when `model_changed` carried this id; a switch held for compaction
    // stays `accepted` until it lands.
    let mut data = object(model);
    data.insert("systemPromptName".into(), json!(system_prompt_name));
    if let Some(status) = ledger.status(effect_id, None) {
        data.extend(object(effect_answer(
            &status,
            ctx.session_id.as_deref(),
            false,
        )));
    }
    respond(id, "set_model", Some(Value::Object(data)))
}

pub async fn run_set_thinking_level(ctx: &EffectCommandContext, command: &Value) -> Value {
    let id = &command["id"];
    let effect_id = command["effectId"].as_str();
    let level = command["level"].as_str().unwrap_or_default();
    match begin_if_effect(ctx, EffectCommandKind::SetThinkingLevel, command).await {
        Err(response) => return response,
        Ok(Some(EffectBegin::Replay(status))) => {
            return respond(
                id,
                "set_thinking_level",
                Some(effect_answer(&status, ctx.session_id.as_deref(), true)),
            );
        }
        Ok(_) => {}
    }
    let ledger = match effect_id {
        Some(_) => Some(EffectLedger::for_session(&ctx.session).await),
        None => None,
    };
    let turn_scope = command["scope"] == "turn";
    // Validate BEFORE mutating: the session clamps an unsupported level, so applying first would leave a
    // rejected request's clamped level in place.
    if turn_scope
        && !ctx
            .session
            .available_thinking_levels()
            .iter()
            .any(|available| available == level)
    {
        let message = unsupported_thinking_level(&command["level"]);
        let failed = transitioned(
            ledger.as_ref(),
            effect_id,
            EffectState::Failed,
            EffectPatch {
                outcome: Some(unsupported_thinking_level_outcome(&message)),
                ..Default::default()
            },
        );
        return refuse(
            id,
            "set_thinking_level",
            &message,
            None,
            failed.map(|failed| effect_answer(&failed, ctx.session_id.as_deref(), false)),
        );
    }
    ctx.session
        .set_thinking_level(level, turn_scope, effect_id.map(str::to_owned));
    match transitioned(
        ledger.as_ref(),
        effect_id,
        EffectState::Done,
        EffectPatch {
            disposition: Some("applied".into()),
            ..Default::default()
        },
    ) {
        Some(done) => respond(
            id,
            "set_thinking_level",
            Some(effect_answer(&done, ctx.session_id.as_deref(), false)),
        ),
        None => respond(id, "set_thinking_level", None),
    }
}

/// `get_effect_status` on a loaded session; a `sessionPath` naming another transcript reads that file.
pub async fn run_get_effect_status(ctx: &EffectCommandContext, command: &Value) -> Value {
    let effect_id = command["effectId"].as_str().unwrap_or_default();
    if let Some(path) = command["sessionPath"].as_str()
        && Some(path) != ctx.session.session_file().as_deref()
    {
        return answer_effect_from_transcript(&command["id"], path, effect_id).await;
    }
    let status = EffectLedger::for_session(&ctx.session)
        .await
        .status(effect_id, ctx.session_id.as_deref());
    respond(
        &command["id"],
        "get_effect_status",
        Some(match status {
            None => json!({"found": false, "searched": "complete"}),
            Some(status) => json!({"found": true, "effect": status}),
        }),
    )
}

/// `get_effect_status { sessionPath }` for a session that is not loaded: an authoritative disk read, run
/// on the blocking pool.
pub async fn answer_effect_from_transcript(id: &Value, path: &str, effect_id: &str) -> Value {
    let (owned_path, owned_effect_id) = (std::path::PathBuf::from(path), effect_id.to_owned());
    let lookup = tokio::task::spawn_blocking(move || {
        lookup_effect_in_transcript(&owned_path, &owned_effect_id)
    })
    .await
    .unwrap_or_else(|join| {
        Err(TranscriptUnreadable {
            path: path.into(),
            reason: join.to_string(),
        })
    });
    match lookup {
        Ok(EffectLookup::Found(effect)) => respond(
            id,
            "get_effect_status",
            Some(json!({"found": true, "effect": effect})),
        ),
        Ok(EffectLookup::NotFoundComplete) => respond(
            id,
            "get_effect_status",
            Some(json!({"found": false, "searched": "complete"})),
        ),
        Err(unreadable) => refuse(
            id,
            "get_effect_status",
            &unreadable.to_string(),
            Some(RPC_ERROR_TRANSCRIPT_UNREADABLE),
            Some(json!({
                "path": unreadable.path,
                "reason": unreadable.reason,
                "action": TranscriptUnreadable::ACTION,
            })),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client_admissions::test_session::FakeSession;
    use crate::effect_session::QueuedInputView;
    use futures::FutureExt;

    fn context(route: &str) -> (Arc<FakeSession>, EffectCommandContext) {
        let fake = Arc::new(FakeSession::new(route));
        let session: Arc<dyn EffectSession> = fake.clone();
        (
            fake,
            EffectCommandContext {
                session,
                by: EffectActor {
                    connection_id: "c1".into(),
                },
                session_id: Some("rpc-1".into()),
            },
        )
    }

    fn forget(route: &str) {
        EffectLedger::forget(route);
        ClientAdmissions::forget(route);
    }

    #[tokio::test]
    async fn abort_validates_grace_and_replays_a_same_id_retry() {
        let (_fake, ctx) = context("ec-abort");
        let invalid = run_abort(
            &ctx,
            &json!({"id":"a","type":"abort","graceMs":60_001}),
            async {}.boxed(),
        )
        .await;
        assert_eq!(invalid["errorCode"], "invalid_argument");
        assert_eq!(invalid["errorData"]["reason"], "invalid_grace_ms");
        let command = json!({"id":"a","type":"abort","effectId":"e1","awaitIdle":true});
        let first = run_abort(&ctx, &command, async {}.boxed()).await;
        assert_eq!(first["data"]["state"], "done");
        assert_eq!(first["data"]["effect"]["disposition"], "no_active_turn");
        assert_eq!(first["data"]["idle"], true);
        assert_eq!(first["data"]["effect"]["sessionId"], "rpc-1");
        let replay = run_abort(
            &ctx,
            &command,
            async {
                unreachable!("a replay never dispatches");
            }
            .boxed(),
        )
        .await;
        assert_eq!(replay["data"]["replayed"], true);
        let conflict = run_abort(
            &ctx,
            &json!({"id":"a","type":"abort","effectId":"e1","clearQueue":true}),
            async {}.boxed(),
        )
        .await;
        assert_eq!(conflict["errorCode"], "effect_id_conflict");
        forget("ec-abort");
    }

    #[tokio::test(start_paused = true)]
    async fn an_await_idle_abort_reports_not_stopped_when_the_grace_runs_out() {
        let (fake, ctx) = context("ec-grace");
        fake.streaming
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let answer = run_abort(
            &ctx,
            &json!({"type":"abort","effectId":"e1","awaitIdle":true,"graceMs":5}),
            futures::future::pending().boxed(),
        )
        .await;
        assert_eq!(answer["data"]["state"], "applied");
        assert_eq!(answer["data"]["stopped"], false);
        forget("ec-grace");
    }

    #[tokio::test]
    async fn set_model_records_unknown_models_and_typed_refusals() {
        let (fake, ctx) = context("ec-model");
        let unknown = run_set_model(
            &ctx,
            &json!({"id":"m","type":"set_model","provider":"x","modelId":"y","effectId":"e1"}),
        )
        .await;
        assert_eq!(unknown["errorCode"], "model_not_found");
        assert_eq!(unknown["errorData"]["state"], "failed");
        *fake.set_model_error.lock().unwrap() =
            Some(SetModelError::ContextBudget("too small".into()));
        let budget = run_set_model(
            &ctx,
            &json!({"type":"set_model","provider":"anthropic","modelId":"claude","effectId":"e2"}),
        )
        .await;
        assert_eq!(budget["errorCode"], "context_budget");
        assert_eq!(
            budget["errorData"]["effect"]["outcome"]["kind"],
            "context_budget"
        );
        *fake.set_model_error.lock().unwrap() = None;
        let held = run_set_model(
            &ctx,
            &json!({"type":"set_model","provider":"anthropic","modelId":"claude","effectId":"e3"}),
        )
        .await;
        assert_eq!(held["success"], true);
        assert_eq!(held["data"]["state"], "accepted");
        assert_eq!(held["data"]["provider"], "anthropic");
        forget("ec-model");
    }

    #[tokio::test]
    async fn a_turn_scoped_unsupported_thinking_level_fails_before_mutating() {
        let (fake, ctx) = context("ec-thinking");
        let refused = run_set_thinking_level(
            &ctx,
            &json!({"type":"set_thinking_level","level":"xhigh","scope":"turn","effectId":"e1"}),
        )
        .await;
        assert_eq!(refused["success"], false);
        assert!(fake.thinking.lock().unwrap().is_empty());
        let applied = run_set_thinking_level(
            &ctx,
            &json!({"type":"set_thinking_level","level":"high","effectId":"e2"}),
        )
        .await;
        assert_eq!(applied["data"]["effect"]["disposition"], "applied");
        assert_eq!(
            fake.thinking.lock().unwrap().as_slice(),
            [("high".to_owned(), false, Some("e2".to_owned()))]
        );
        forget("ec-thinking");
    }

    #[tokio::test]
    async fn queued_changes_answer_and_replay_the_same_receipt() {
        let (fake, ctx) = context("ec-queue");
        fake.queued.lock().unwrap().push(QueuedInputView {
            queued_id: "q1".into(),
            client_message_id: None,
            text: "old".into(),
            revision: 1,
        });
        let command =
            json!({"type":"edit_queued_message","queuedId":"q1","text":"new","effectId":"e1"});
        let edited = run_queued_input_change(&ctx, &command).await;
        assert_eq!(edited["data"]["status"], "edited");
        assert_eq!(edited["data"]["text"], "new");
        assert_eq!(edited["data"]["revision"], 2);
        let replay = run_queued_input_change(&ctx, &command).await;
        assert_eq!(replay["data"]["status"], "edited");
        assert_eq!(replay["data"]["revision"], 2);
        assert_eq!(replay["data"]["replayed"], true);
        let missing = run_queued_input_change(
            &ctx,
            &json!({"type":"dequeue_message","queuedId":"gone","effectId":"e2"}),
        )
        .await;
        assert_eq!(missing["data"]["status"], "not_found");
        assert_eq!(missing["data"]["reason"], "not_found");
        assert_eq!(missing["data"]["state"], "failed");
        forget("ec-queue");
    }

    #[tokio::test]
    async fn get_effect_status_answers_loaded_and_unreadable_transcripts() {
        let (_fake, ctx) = context("ec-status");
        let missing =
            run_get_effect_status(&ctx, &json!({"type":"get_effect_status","effectId":"nope"}))
                .await;
        assert_eq!(
            missing["data"],
            json!({"found":false,"searched":"complete"})
        );
        let unreadable = run_get_effect_status(
            &ctx,
            &json!({"type":"get_effect_status","effectId":"e","sessionPath":"/nonexistent/x.jsonl"}),
        )
        .await;
        assert_eq!(unreadable["errorCode"], "transcript_unreadable");
        forget("ec-status");
    }
}
