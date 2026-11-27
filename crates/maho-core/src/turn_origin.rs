//! Port of senpi packages/coding-agent/src/core/turn-origin.ts.
//!
//! Tracks who started a turn (`local`, `delegated`, `engine`) across async task boundaries and
//! session runs (`senpi#6342`).

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::{Arc, LazyLock, Mutex};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnOrigin {
    Local,
    Delegated,
    Engine,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputTurnMark {
    Local,
    Delegated,
    Engine,
    Join,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionOrigin {
    Local,
    Delegated,
}

#[derive(Clone, Copy, Debug)]
struct ScopeState {
    origin: ConnectionOrigin,
    open: bool,
}

tokio::task_local! {
    static RECORD_SCOPE: Arc<Mutex<ScopeState>>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SessionOriginState {
    origin: TurnOrigin,
    person_turn: Option<u64>,
}

static NEXT_PERSON_TURN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
static CURRENT_ORIGINS: LazyLock<Mutex<HashMap<String, SessionOriginState>>> =
    LazyLock::new(Default::default);
static CONTINUING_TURNS: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Default::default);

pub fn input_turn_mark(
    mode: Option<&str>,
    connection_origin: Option<ConnectionOrigin>,
    source: Option<&str>,
) -> InputTurnMark {
    if mode == Some("steer") {
        return InputTurnMark::Join;
    }
    if let Some(conn) = connection_origin {
        return match conn {
            ConnectionOrigin::Local => InputTurnMark::Local,
            ConnectionOrigin::Delegated => InputTurnMark::Delegated,
        };
    }
    if record_is_delegated() {
        return InputTurnMark::Delegated;
    }
    if source.is_none() || source == Some("interactive") {
        InputTurnMark::Local
    } else {
        InputTurnMark::Engine
    }
}

pub async fn run_in_record_scope<T, F>(origin: ConnectionOrigin, handle: F) -> T
where
    F: Future<Output = T>,
{
    let scope = Arc::new(Mutex::new(ScopeState { origin, open: true }));
    RECORD_SCOPE.scope(scope, handle).await
}

pub fn record_is_delegated() -> bool {
    RECORD_SCOPE
        .try_with(|scope| {
            let guard = scope
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.open && guard.origin == ConnectionOrigin::Delegated
        })
        .unwrap_or(false)
}

fn close_delegated_at_run_start() -> bool {
    RECORD_SCOPE
        .try_with(|scope| {
            let mut guard = scope
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let opened = guard.open && guard.origin == ConnectionOrigin::Delegated;
            guard.open = false;
            opened
        })
        .unwrap_or(false)
}

pub fn note_run_start(session_key: &str) {
    if CONTINUING_TURNS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(session_key)
    {
        return;
    }
    let origin = if close_delegated_at_run_start() {
        TurnOrigin::Delegated
    } else {
        TurnOrigin::Engine
    };
    CURRENT_ORIGINS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(
            session_key.to_owned(),
            SessionOriginState {
                origin,
                person_turn: None,
            },
        );
}

pub fn note_turn_continuation(session_key: &str) {
    CONTINUING_TURNS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(session_key.to_owned());
}

pub fn end_turn_continuation(session_key: &str) {
    CONTINUING_TURNS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(session_key);
}

pub fn note_user_message_start(
    session_key: &str,
    mark: Option<InputTurnMark>,
    person_started: bool,
) {
    if mark == Some(InputTurnMark::Join) {
        return;
    }
    let origin = match mark {
        Some(InputTurnMark::Local) => TurnOrigin::Local,
        Some(InputTurnMark::Delegated) => TurnOrigin::Delegated,
        Some(InputTurnMark::Engine) | None => TurnOrigin::Engine,
        Some(InputTurnMark::Join) => return,
    };
    let person_turn =
        person_started.then(|| NEXT_PERSON_TURN.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
    CURRENT_ORIGINS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(
            session_key.to_owned(),
            SessionOriginState {
                origin,
                person_turn,
            },
        );
}

pub fn current_person_turn(session_key: &str) -> Option<u64> {
    CURRENT_ORIGINS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(session_key)
        .and_then(|s| s.person_turn)
}

pub fn deciding_turn_is_delegated(session_key: &str) -> bool {
    CURRENT_ORIGINS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(session_key)
        .is_some_and(|s| s.origin == TurnOrigin::Delegated)
}

const INPUT_TURN_MARK_KEY: &str = "__input_turn_mark";

pub fn mark_input_turn_origin(message: &mut serde_json::Value, mark: InputTurnMark) {
    if let Some(obj) = message.as_object_mut()
        && let Ok(val) = serde_json::to_value(mark)
    {
        obj.insert(INPUT_TURN_MARK_KEY.to_owned(), val);
    }
}

pub fn take_input_turn_mark(message: &mut serde_json::Value) -> Option<InputTurnMark> {
    message
        .as_object_mut()
        .and_then(|obj| obj.remove(INPUT_TURN_MARK_KEY))
        .and_then(|val| serde_json::from_value(val).ok())
}

pub fn read_input_turn_mark(message: &serde_json::Value) -> Option<InputTurnMark> {
    message
        .get(INPUT_TURN_MARK_KEY)
        .and_then(|val| serde_json::from_value(val.clone()).ok())
}
