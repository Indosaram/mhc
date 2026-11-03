//! `unload_session` (senpi `modes/rpc/session-unload.ts`): free a session this host keeps loaded, keeping its
//! transcript for a later `open_session { sessionPath }` (senpi#3225).
//!
//! An unload is the idle sweep's park on request: the same teardown, sealed as `session_parked { reason: "unloaded"
//! }`. Queued input stays in the admission ledger, which restores it on the reopen (id-less input too, senpi#1971),
//! unless `clearQueue`. Unlike `release_session` the caller is expected to be attached, and nothing hands the file
//! to another runtime.
//!
//! Refusals change nothing and answer `outcome: "refused"` with `reason`, `detail` and `action`:
//! - `turn_active` / `work_pending`: an agent run, a prompt in preflight, compaction, barrier-held work, bash, an
//!   admitted delivery or another request for the session. `interrupt` aborts the run and bash and waits (bounded)
//!   before the final check.
//! - `extension_work`: a published wake source other than child tasks. The teardown stops that work and most of it
//!   does not come back on reopen, so only `force` unloads past it; the refusal names the sources.
//! - `children_live`: the session started child tasks that are still working (a `*-task` wake source) or worker
//!   sessions here name it as their parent or root. `children: "detach"` leaves them running.
//! - `attached`: other clients hold the session. `force` unloads anyway; they receive the park.
//!
//! The last check runs right before the close claim; the host's teardown re-checks the entry is still open under the
//! registry lock, so a close that won in between answers `session_closing`. ADAPTATION: the router's registry sits
//! behind an async lock, so the lookup awaits.
use crate::client_admissions::ClientAdmissions;
use crate::effect_commands::{EffectCommandContext, begin_effect, effect_answer};
use crate::effect_ledger::{EffectBegin, EffectLedger, EffectPatch};
use crate::effect_record::{
    EffectActor, EffectCommandKind, EffectOutcome, EffectOutcomeKind, EffectState,
};
use crate::effect_session::EffectSession;
use crate::session_release_interrupt::{ReleasableSession, ReleaseSettlePort, abort_and_settle};
use async_trait::async_trait;
use serde_json::{Map, Value, json};
use std::sync::Arc;

/// The live session an unload reads and drives: the release slice plus the wake sources it publishes, its durable
/// id, and the effect ledger's view of it.
pub trait UnloadableSession: ReleasableSession {
    /// `session.activeWakeSources`.
    fn active_wake_sources(&self) -> Vec<String>;
    /// `sessionManager.getSessionId()`.
    fn durable_session_id(&self) -> String;
    /// `sessionFile`.
    fn session_file(&self) -> Option<String>;
    /// The effect ledger's view of this session; `None` refuses an `effectId` unload as unsupported.
    fn effect_session(&self) -> Option<Arc<dyn EffectSession>>;
}

/// What the router lends a live entry.
pub struct UnloadEntry {
    /// `entry.state === "open"`.
    pub open: bool,
    /// `entry.runtime?.session`; `None` when a worker isolate owns the runtime.
    pub session: Option<Arc<dyn UnloadableSession>>,
    pub session_path: Option<String>,
}

/// senpi `SessionUnloadPort`.
#[async_trait]
pub trait SessionUnloadPort: ReleaseSettlePort {
    fn draining(&self) -> bool;
    fn by(&self) -> EffectActor;
    /// The live entry, or the wire code of the lookup failure.
    async fn lookup(&self, session_id: &str) -> Result<UnloadEntry, String>;
    /// Requests for the session in flight on any connection, the unload itself not counted.
    fn other_requests(&self, session_id: &str) -> usize;
    /// Attachments held by connections other than the caller's.
    fn other_attachments(&self, session_id: &str) -> u64;
    /// Open worker sessions on this host whose context names `durable_session_id` as parent or root.
    async fn open_child_sessions(&self, durable_session_id: &str) -> usize;
    /// The park teardown, sealed `session_parked { reason: "unloaded" }`. `false` when another close owns it.
    async fn tear_down(&self, session_id: &str, session_path: &str) -> bool;
}

/// Wake sources whose live work is child tasks this session started (`/^senpi-task/`; maho-omo-task publishes
/// under the same name).
fn is_child_task_source(source: &str) -> bool {
    source.starts_with("senpi-task")
}

#[derive(Debug, Clone, PartialEq)]
struct Refusal {
    reason: &'static str,
    detail: String,
    action: Option<&'static str>,
}

const ACTION_EXTENSION_WORK: &str =
    "let the named work finish, or unload with force: true (it is stopped, not resumed on reopen)";
const ACTION_TURN_ACTIVE: &str =
    "wait for the turn to end, or unload with interrupt: true to stop it first";
const ACTION_WORK_PENDING: &str = "wait for the work to finish, or unload with interrupt: true";
const ACTION_CHILDREN_LIVE: &str =
    "wait for the child tasks, or unload with children: \"detach\" to leave them running";
const ACTION_ATTACHED: &str =
    "ask the other clients to detach, or unload with force: true (they receive session_parked)";

struct Loaded {
    session: Arc<dyn UnloadableSession>,
    session_path: String,
    durable_session_id: String,
}

/// senpi `unloadSession`.
pub async fn unload_session(port: &dyn SessionUnloadPort, command: &Value) -> Value {
    let session_id = command["sessionId"].as_str().unwrap_or_default();
    if port.draining() {
        return answer(
            command,
            refused(&Refusal {
                reason: "host_draining",
                detail: "this host is draining and parks every session itself".into(),
                action: None,
            }),
        );
    }
    let first = match loaded(port, session_id).await {
        Ok(first) => first,
        Err(refusal) => return answer(command, refused(&refusal)),
    };
    if let Some(effect_id) = command["effectId"].as_str() {
        let Some(effects) = first.session.effect_session() else {
            return answer(
                command,
                refused(&Refusal {
                    reason: "unsupported",
                    detail: "this session keeps no effect ledger".into(),
                    action: None,
                }),
            );
        };
        let ctx = EffectCommandContext {
            session: effects,
            by: port.by(),
            session_id: Some(session_id.to_owned()),
        };
        match begin_effect(&ctx, EffectCommandKind::Unload, command, effect_id).await {
            Err(error) => return failure(command, &error.to_string()),
            Ok(Err(response)) => return response,
            Ok(Ok(EffectBegin::Replay(replay))) => {
                let mut data = Map::new();
                let outcome = if replay.state == EffectState::Failed {
                    "refused"
                } else {
                    "unloaded"
                };
                data.insert("outcome".into(), outcome.into());
                // A replayed refusal answers the same reason, detail and action as the first one.
                if let Some(outcome) = &replay.outcome
                    && let Some(reason) = &outcome.reason
                {
                    data.insert("reason".into(), json!(reason));
                    if let Some(detail) = &outcome.detail {
                        data.insert("detail".into(), json!(detail));
                    }
                    if let Some(action) = &outcome.action {
                        data.insert("action".into(), json!(action));
                    }
                }
                extend(&mut data, effect_answer(&replay, Some(session_id), true));
                return answer(command, data);
            }
            Ok(Ok(EffectBegin::Record(_))) => {}
        }
    }
    let blocked = blockers(port, command, &first).await;
    // Never stop work for an unload that would still be refused: only interruptible blockers are interrupted.
    if let Some(standing) = blocked
        .iter()
        .find(|refusal| !interruptible(refusal) || command["interrupt"] != true)
    {
        return fail(command, &first, standing).await;
    }
    if !blocked.is_empty() {
        let session: Arc<dyn ReleasableSession> = first.session.clone();
        abort_and_settle(port, session_id, &session).await;
    }
    claim_and_unload(port, command).await
}

/// The final check and the unload, without an await between them and the close claim.
async fn claim_and_unload(port: &dyn SessionUnloadPort, command: &Value) -> Value {
    let session_id = command["sessionId"].as_str().unwrap_or_default();
    let ready = match loaded(port, session_id).await {
        Ok(ready) => ready,
        Err(refusal) => return answer(command, refused(&refusal)),
    };
    let still_blocked = blockers(port, command, &ready).await;
    if let Some(first) = still_blocked.first() {
        return fail(command, &ready, first).await;
    }
    if command["clearQueue"] == true {
        if let Some(effects) = ready.session.effect_session() {
            ClientAdmissions::clear_queued_for(&effects.effect_route());
        }
        ready.session.clear_queue(false);
    }
    let effect = match (command["effectId"].as_str(), ready.session.effect_session()) {
        (Some(effect_id), Some(effects)) => EffectLedger::for_session(&effects)
            .await
            .transition(
                effect_id,
                EffectState::Done,
                EffectPatch {
                    disposition: Some("unloaded".into()),
                    ..Default::default()
                },
            )
            .ok(),
        _ => None,
    };
    if !port.tear_down(session_id, &ready.session_path).await {
        return answer(
            command,
            refused(&Refusal {
                reason: "session_closing",
                detail: "another close of this session finished first".into(),
                action: None,
            }),
        );
    }
    let mut data = Map::new();
    data.insert("outcome".into(), "unloaded".into());
    data.insert("sessionPath".into(), json!(ready.session_path));
    if let Some(effect) = &effect {
        extend(&mut data, effect_answer(effect, Some(session_id), false));
    }
    answer(command, data)
}

fn interruptible(refusal: &Refusal) -> bool {
    matches!(refusal.reason, "turn_active" | "work_pending")
}

async fn blockers(port: &dyn SessionUnloadPort, command: &Value, loaded: &Loaded) -> Vec<Refusal> {
    let session_id = command["sessionId"].as_str().unwrap_or_default();
    let session = &loaded.session;
    let mut found = Vec::new();
    let activity = session.activity_snapshot();
    let running: Vec<&str> = [
        (activity.is_streaming, "an agent run"),
        (
            !port.pending_prompts(session_id).is_empty(),
            "a prompt about to start",
        ),
        (activity.is_compacting, "compaction"),
        (activity.has_session_work, "session work"),
    ]
    .into_iter()
    .filter_map(|(on, label)| on.then_some(label))
    .collect();
    if !running.is_empty() {
        found.push(Refusal {
            reason: "turn_active",
            detail: running.join(", "),
            action: Some(ACTION_TURN_ACTIVE),
        });
    }
    let pending: Vec<&str> = [
        (activity.is_bash_running, "a bash command"),
        (
            !session.admission_pending().is_empty(),
            "an admitted delivery not yet written",
        ),
        (
            port.other_requests(session_id) > 0,
            "another request for this session",
        ),
    ]
    .into_iter()
    .filter_map(|(on, label)| on.then_some(label))
    .collect();
    if !pending.is_empty() {
        found.push(Refusal {
            reason: "work_pending",
            detail: pending.join(", "),
            action: Some(ACTION_WORK_PENDING),
        });
    }
    let force = command["force"] == true;
    let (child_sources, other_sources): (Vec<String>, Vec<String>) = session
        .active_wake_sources()
        .into_iter()
        .partition(|source| is_child_task_source(source));
    if !other_sources.is_empty() && !force {
        found.push(Refusal {
            reason: "extension_work",
            detail: format!("work published by {}", other_sources.join(", ")),
            action: Some(ACTION_EXTENSION_WORK),
        });
    }
    let child_sessions = port.open_child_sessions(&loaded.durable_session_id).await;
    if command["children"] != "detach" && (!child_sources.is_empty() || child_sessions > 0) {
        let what = if child_sessions > 0 {
            format!("{child_sessions} child session(s) open on this host")
        } else {
            "child tasks still working".to_owned()
        };
        found.push(Refusal {
            reason: "children_live",
            detail: what,
            action: Some(ACTION_CHILDREN_LIVE),
        });
    }
    let others = port.other_attachments(session_id);
    if others > 0 && !force {
        found.push(Refusal {
            reason: "attached",
            detail: format!("{others} other client(s) attached"),
            action: Some(ACTION_ATTACHED),
        });
    }
    found
}

async fn loaded(port: &dyn SessionUnloadPort, session_id: &str) -> Result<Loaded, Refusal> {
    let entry = port.lookup(session_id).await.map_err(|code| Refusal {
        reason: "unsupported",
        detail: code,
        action: None,
    })?;
    if !entry.open {
        return Err(Refusal {
            reason: "session_closing",
            detail: "the session is already closing".into(),
            action: None,
        });
    }
    let Some(session) = entry.session else {
        return Err(Refusal {
            reason: "unsupported",
            detail: "a worker isolate owns this session's runtime".into(),
            action: None,
        });
    };
    let Some(session_path) = entry.session_path.or_else(|| session.session_file()) else {
        return Err(Refusal {
            reason: "unsupported",
            detail: "the session has no file to reopen it by".into(),
            action: Some("send a prompt first so the session is written, or close it instead"),
        });
    };
    let durable_session_id = session.durable_session_id();
    Ok(Loaded {
        session,
        session_path,
        durable_session_id,
    })
}

/// A refusal after the effect began records it `failed`; the answer carries the receipt.
async fn fail(command: &Value, loaded: &Loaded, refusal: &Refusal) -> Value {
    let session_id = command["sessionId"].as_str().unwrap_or_default();
    let (Some(effect_id), Some(effects)) = (
        command["effectId"].as_str(),
        loaded.session.effect_session(),
    ) else {
        return answer(command, refused(refusal));
    };
    let status = EffectLedger::for_session(&effects).await.transition(
        effect_id,
        EffectState::Failed,
        EffectPatch {
            outcome: Some(EffectOutcome {
                kind: EffectOutcomeKind::Refused,
                reason: Some(refusal.reason.to_owned()),
                detail: Some(refusal.detail.clone()),
                action: refusal.action.map(str::to_owned),
                abort_source: None,
                aborted_by: None,
            }),
            ..Default::default()
        },
    );
    let mut data = refused(refusal);
    if let Ok(status) = &status {
        extend(&mut data, effect_answer(status, Some(session_id), false));
    }
    answer(command, data)
}

fn refused(refusal: &Refusal) -> Map<String, Value> {
    let mut data = Map::new();
    data.insert("outcome".into(), "refused".into());
    data.insert("reason".into(), refusal.reason.into());
    data.insert("detail".into(), json!(refusal.detail));
    if let Some(action) = refusal.action {
        data.insert("action".into(), action.into());
    }
    data
}

fn extend(data: &mut Map<String, Value>, value: Value) {
    if let Value::Object(fields) = value {
        data.extend(fields);
    }
}

fn answer(command: &Value, data: Map<String, Value>) -> Value {
    let mut response = Map::new();
    if let Some(id) = command.get("id").filter(|id| !id.is_null()) {
        response.insert("id".into(), id.clone());
    }
    response.insert("type".into(), "response".into());
    response.insert("command".into(), "unload_session".into());
    response.insert("success".into(), true.into());
    response.insert("data".into(), Value::Object(data));
    Value::Object(response)
}

fn failure(command: &Value, error: &str) -> Value {
    let mut response = Map::new();
    if let Some(id) = command.get("id").filter(|id| !id.is_null()) {
        response.insert("id".into(), id.clone());
    }
    response.insert("type".into(), "response".into());
    response.insert("command".into(), "unload_session".into());
    response.insert("success".into(), false.into());
    response.insert("error".into(), error.into());
    Value::Object(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client_admissions::test_session::FakeSession;
    use crate::session_release_interrupt::{BoxFuture, ReleaseWriteError, SubscriptionGuard};
    use maho_core::agent_session::ClearedQueue;
    use maho_core::session_activity::SessionActivitySnapshot;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct Live {
        activity: Mutex<SessionActivitySnapshot>,
        sources: Mutex<Vec<String>>,
        aborts: AtomicUsize,
        cleared: AtomicUsize,
        effects: Mutex<Option<Arc<FakeSession>>>,
    }

    struct Handle(Arc<Live>);

    #[async_trait]
    impl ReleasableSession for Handle {
        fn activity_snapshot(&self) -> SessionActivitySnapshot {
            *self.0.activity.lock().unwrap()
        }
        fn pending_message_count(&self) -> usize {
            0
        }
        fn admission_pending(&self) -> Vec<String> {
            Vec::new()
        }
        fn admission_close(&self, _: &str) {}
        fn admission_reopen(&self) {}
        fn clear_queue(&self, _: bool) -> ClearedQueue {
            self.0.cleared.fetch_add(1, Ordering::SeqCst);
            ClearedQueue {
                steering: Vec::new(),
                follow_up: Vec::new(),
                ordered: Vec::new(),
            }
        }
        fn abort_bash(&self) {}
        fn on_agent_start(&self, _: Arc<dyn Fn() + Send + Sync>) -> SubscriptionGuard {
            Box::new(())
        }
        async fn abort(&self) {
            self.0.aborts.fetch_add(1, Ordering::SeqCst);
            *self.0.activity.lock().unwrap() = SessionActivitySnapshot::default();
        }
        async fn wait_for_idle(&self) {}
        async fn when_queue_clear_recorded(&self) {}
        fn session_id(&self) -> String {
            "durable".into()
        }
        fn is_transcript_flushed(&self) -> bool {
            true
        }
        async fn persist_header_now(&self) -> Result<(), String> {
            Ok(())
        }
        fn append_custom_entry(&self, _: &str, _: Value) -> Result<(), ReleaseWriteError> {
            Ok(())
        }
    }

    impl UnloadableSession for Handle {
        fn active_wake_sources(&self) -> Vec<String> {
            self.0.sources.lock().unwrap().clone()
        }
        fn durable_session_id(&self) -> String {
            "durable".into()
        }
        fn session_file(&self) -> Option<String> {
            None
        }
        fn effect_session(&self) -> Option<Arc<dyn EffectSession>> {
            self.0
                .effects
                .lock()
                .unwrap()
                .clone()
                .map(|fake| fake as Arc<dyn EffectSession>)
        }
    }

    struct Port {
        live: Arc<Live>,
        draining: bool,
        open: bool,
        path: Option<String>,
        other_attachments: u64,
        children: usize,
        torn_down: AtomicUsize,
        tear_down_wins: bool,
    }

    fn port() -> Port {
        Port {
            live: Arc::new(Live::default()),
            draining: false,
            open: true,
            path: Some("/s.jsonl".into()),
            other_attachments: 0,
            children: 0,
            torn_down: AtomicUsize::new(0),
            tear_down_wins: true,
        }
    }

    #[async_trait]
    impl ReleaseSettlePort for Port {
        async fn other_requests_settled(&self, _: &str) {}
        fn pending_prompts(&self, _: &str) -> Vec<BoxFuture<'static, ()>> {
            Vec::new()
        }
    }

    #[async_trait]
    impl SessionUnloadPort for Port {
        fn draining(&self) -> bool {
            self.draining
        }
        fn by(&self) -> EffectActor {
            EffectActor {
                connection_id: "c1".into(),
            }
        }
        async fn lookup(&self, session_id: &str) -> Result<UnloadEntry, String> {
            if session_id != "rpc-1" {
                return Err("unknown_session".into());
            }
            Ok(UnloadEntry {
                open: self.open,
                session: Some(
                    Arc::new(Handle(Arc::clone(&self.live))) as Arc<dyn UnloadableSession>
                ),
                session_path: self.path.clone(),
            })
        }
        fn other_requests(&self, _: &str) -> usize {
            0
        }
        fn other_attachments(&self, _: &str) -> u64 {
            self.other_attachments
        }
        async fn open_child_sessions(&self, durable: &str) -> usize {
            assert_eq!(durable, "durable");
            self.children
        }
        async fn tear_down(&self, _: &str, path: &str) -> bool {
            assert_eq!(path, "/s.jsonl");
            self.torn_down.fetch_add(1, Ordering::SeqCst);
            self.tear_down_wins
        }
    }

    fn unload(fields: Value) -> Value {
        let mut command = json!({"id":"u","type":"unload_session","sessionId":"rpc-1"});
        for (key, value) in fields.as_object().into_iter().flatten() {
            command[key] = value.clone();
        }
        command
    }

    #[tokio::test]
    async fn an_idle_session_unloads_and_answers_its_path() {
        let port = port();
        let answer = unload_session(&port, &unload(json!({}))).await;
        assert_eq!(
            answer,
            json!({"id":"u","type":"response","command":"unload_session","success":true,
                "data":{"outcome":"unloaded","sessionPath":"/s.jsonl"}})
        );
        assert_eq!(port.torn_down.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_running_turn_is_refused_with_an_action_then_unloaded_with_interrupt() {
        let port = port();
        port.live.activity.lock().unwrap().is_streaming = true;
        let refused = unload_session(&port, &unload(json!({}))).await;
        assert_eq!(refused["data"]["outcome"], "refused");
        assert_eq!(refused["data"]["reason"], "turn_active");
        assert!(
            refused["data"]["action"]
                .as_str()
                .unwrap()
                .contains("interrupt")
        );
        assert_eq!(port.torn_down.load(Ordering::SeqCst), 0);
        let interrupted = unload_session(&port, &unload(json!({"interrupt":true}))).await;
        assert_eq!(interrupted["data"]["outcome"], "unloaded");
        assert_eq!(port.live.aborts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn extension_work_is_refused_even_with_interrupt_and_unloaded_only_with_force() {
        let port = port();
        *port.live.sources.lock().unwrap() = vec!["terminal".into()];
        let refused = unload_session(&port, &unload(json!({"interrupt":true}))).await;
        assert_eq!(refused["data"]["reason"], "extension_work");
        assert_eq!(refused["data"]["detail"], "work published by terminal");
        assert_eq!(port.live.aborts.load(Ordering::SeqCst), 0);
        let forced = unload_session(&port, &unload(json!({"force":true}))).await;
        assert_eq!(forced["data"]["outcome"], "unloaded");
    }

    #[tokio::test]
    async fn live_children_are_refused_unless_detached() {
        let mut port = port();
        port.children = 2;
        let refused = unload_session(&port, &unload(json!({}))).await;
        assert_eq!(refused["data"]["reason"], "children_live");
        assert_eq!(
            refused["data"]["detail"],
            "2 child session(s) open on this host"
        );
        let detached = unload_session(&port, &unload(json!({"children":"detach"}))).await;
        assert_eq!(detached["data"]["outcome"], "unloaded");
        // A child-task wake source counts as live children, not extension work.
        port.children = 0;
        *port.live.sources.lock().unwrap() = vec!["senpi-task".into()];
        let refused = unload_session(&port, &unload(json!({}))).await;
        assert_eq!(refused["data"]["reason"], "children_live");
        assert_eq!(refused["data"]["detail"], "child tasks still working");
    }

    #[tokio::test]
    async fn other_clients_are_refused_unless_forced() {
        let mut port = port();
        port.other_attachments = 1;
        let refused = unload_session(&port, &unload(json!({}))).await;
        assert_eq!(refused["data"]["reason"], "attached");
        let forced = unload_session(&port, &unload(json!({"force":true}))).await;
        assert_eq!(forced["data"]["outcome"], "unloaded");
    }

    #[tokio::test]
    async fn draining_closing_and_fileless_sessions_are_refused() {
        let mut port = port();
        port.draining = true;
        assert_eq!(
            unload_session(&port, &unload(json!({}))).await["data"]["reason"],
            "host_draining"
        );
        port.draining = false;
        port.open = false;
        assert_eq!(
            unload_session(&port, &unload(json!({}))).await["data"]["reason"],
            "session_closing"
        );
        port.open = true;
        port.path = None;
        let fileless = unload_session(&port, &unload(json!({}))).await;
        assert_eq!(fileless["data"]["reason"], "unsupported");
        assert!(fileless["data"]["action"].is_string());
        let unknown =
            unload_session(&port, &json!({"type":"unload_session","sessionId":"x"})).await;
        assert_eq!(unknown["data"]["detail"], "unknown_session");
        assert!(unknown.get("id").is_none());
    }

    #[tokio::test]
    async fn losing_the_close_claim_answers_session_closing() {
        let mut port = port();
        port.tear_down_wins = false;
        let answer = unload_session(&port, &unload(json!({}))).await;
        assert_eq!(answer["data"]["reason"], "session_closing");
    }

    #[tokio::test]
    async fn clear_queue_empties_the_queue_before_the_teardown() {
        let port = port();
        unload_session(&port, &unload(json!({"clearQueue":true}))).await;
        assert_eq!(port.live.cleared.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn an_effect_unload_records_done_and_a_same_id_retry_replays_it() {
        let port = port();
        *port.live.effects.lock().unwrap() = Some(Arc::new(FakeSession::new("unload-done")));
        let first = unload_session(&port, &unload(json!({"effectId":"u1"}))).await;
        assert_eq!(first["data"]["outcome"], "unloaded");
        assert_eq!(first["data"]["effectId"], "u1");
        assert_eq!(first["data"]["state"], "done");
        let replay = unload_session(&port, &unload(json!({"effectId":"u1"}))).await;
        assert_eq!(replay["data"]["outcome"], "unloaded");
        assert_eq!(replay["data"]["replayed"], true);
        assert_eq!(port.torn_down.load(Ordering::SeqCst), 1);
        EffectLedger::forget("unload-done");
        ClientAdmissions::forget("unload-done");
    }

    #[tokio::test]
    async fn an_effect_refusal_is_recorded_failed_and_replays_its_reason() {
        let mut port = port();
        port.other_attachments = 1;
        *port.live.effects.lock().unwrap() = Some(Arc::new(FakeSession::new("unload-refused")));
        let first = unload_session(&port, &unload(json!({"effectId":"u2"}))).await;
        assert_eq!(first["data"]["outcome"], "refused");
        assert_eq!(first["data"]["state"], "failed");
        let replay = unload_session(&port, &unload(json!({"effectId":"u2"}))).await;
        assert_eq!(replay["data"]["outcome"], "refused");
        assert_eq!(replay["data"]["reason"], "attached");
        assert_eq!(replay["data"]["action"], ACTION_ATTACHED);
        assert_eq!(replay["data"]["replayed"], true);
        EffectLedger::forget("unload-refused");
        ClientAdmissions::forget("unload-refused");
    }
}
