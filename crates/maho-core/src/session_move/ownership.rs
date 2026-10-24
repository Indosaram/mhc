//! Who may write a session file when sessions move between devices (senpi `core/session-move/ownership.ts`).
//!
//! The file itself records ownership as a sequence of transitions, folded in order:
//! - writable (no ownership entry yet, a `takeover` release, `session_moved_in`, `session_reclaimed`);
//! - `moving` after a `move` release: only that move may continue, with `session_moved` (it arrived),
//!   `session_reclaimed` (it was given back) or `session_moved_in` (an import of that same move);
//! - `moved` after `session_moved`: nothing may reopen it. A new move id arriving by import replaces the file.
//!
//! A transition the current state does not allow is refused when written and ignored when read. Entries name the
//! session they belong to (`session_id`); a copy or branch under a new id ignores the entries it inherited.
use serde_json::Value;

/// `customType` of the release entry; shared with `release_session`.
pub const SESSION_RELEASED: &str = "session_released";
pub const SESSION_MOVED_IN: &str = "session_moved_in";
pub const SESSION_MOVED: &str = "session_moved";
pub const SESSION_RECLAIMED: &str = "session_reclaimed";

const OWNERSHIP_TYPES: [&str; 4] = [
    SESSION_RELEASED,
    SESSION_MOVED_IN,
    SESSION_MOVED,
    SESSION_RECLAIMED,
];

/// `MoveId`: a plain id (see `bundle::is_move_id`).
pub type MoveId = String;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusedState {
    Moving,
    Moved,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefusedOwnership {
    pub state: RefusedState,
    pub move_id: MoveId,
    pub target: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionOwnership {
    Writable,
    Refused(RefusedOwnership),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnershipStep {
    None,
    Apply(SessionOwnership),
    Refused,
}

pub fn is_ownership_entry_type(custom_type: Option<&str>) -> bool {
    custom_type.is_some_and(|value| OWNERSHIP_TYPES.contains(&value))
}

fn field<'a>(data: Option<&'a Value>, key: &str) -> Option<&'a str> {
    data?.as_object()?.get(key)?.as_str()
}

fn requested(entry: &Value) -> SessionOwnership {
    let data = entry.get("data");
    let move_id = field(data, "move_id").unwrap_or_default().to_owned();
    let target = field(data, "target").unwrap_or_default().to_owned();
    let custom_type = entry.get("customType").and_then(Value::as_str);
    if custom_type == Some(SESSION_MOVED) {
        return SessionOwnership::Refused(RefusedOwnership {
            state: RefusedState::Moved,
            move_id,
            target,
        });
    }
    if custom_type == Some(SESSION_RELEASED) && field(data, "reason") == Some("move") {
        return SessionOwnership::Refused(RefusedOwnership {
            state: RefusedState::Moving,
            move_id,
            target,
        });
    }
    SessionOwnership::Writable
}

/// The single transition rule. `session_id` is the id of the session being written; an entry naming another
/// session (inherited by a copy or branch) is not an ownership step for this one.
pub fn ownership_step(
    current: &SessionOwnership,
    entry: &Value,
    session_id: Option<&str>,
) -> OwnershipStep {
    let custom_type = entry.get("customType").and_then(Value::as_str);
    if entry.get("type").and_then(Value::as_str) != Some("custom")
        || !is_ownership_entry_type(custom_type)
    {
        return OwnershipStep::None;
    }
    let data = entry.get("data");
    let owner = field(data, "session_id");
    if let (Some(owner), Some(session_id)) = (owner, session_id)
        && owner != session_id
    {
        return OwnershipStep::None;
    }
    let next = requested(entry);
    match current {
        SessionOwnership::Writable => OwnershipStep::Apply(next),
        SessionOwnership::Refused(refused) if refused.state == RefusedState::Moved => {
            OwnershipStep::Refused
        }
        SessionOwnership::Refused(refused) => {
            let same_move = field(data, "move_id") == Some(refused.move_id.as_str());
            let continues_move = matches!(
                custom_type,
                Some(SESSION_MOVED | SESSION_RECLAIMED | SESSION_MOVED_IN)
            );
            if same_move && continues_move {
                OwnershipStep::Apply(next)
            } else {
                OwnershipStep::Refused
            }
        }
    }
}

pub fn header_session_id(entries: &[Value]) -> Option<&str> {
    entries
        .iter()
        .find(|entry| entry.get("type").and_then(Value::as_str) == Some("session"))?
        .get("id")?
        .as_str()
}

pub fn session_ownership(entries: &[Value]) -> SessionOwnership {
    let session_id = header_session_id(entries);
    let mut verdict = SessionOwnership::Writable;
    for entry in entries {
        if let OwnershipStep::Apply(next) = ownership_step(&verdict, entry, session_id) {
            verdict = next;
        }
    }
    verdict
}

/// `SessionMovedAwayError`: a write refused because the session file is moving or moved elsewhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionMovedAwayError {
    pub ownership: RefusedOwnership,
    message: String,
}

impl SessionMovedAwayError {
    pub const CODE: &'static str = "session_moved_away";

    pub fn new(session_file: &str, ownership: RefusedOwnership) -> Self {
        let place = if ownership.target.is_empty() {
            "another device"
        } else {
            ownership.target.as_str()
        };
        let message = match ownership.state {
            RefusedState::Moving => format!(
                "This session is being moved to {place}; it is read-only here. Wait for the move to finish, or make a copy to keep working here. ({session_file})"
            ),
            RefusedState::Moved => format!(
                "This session was moved to {place}; it is read-only here. Open it on {place}, or make a copy here. ({session_file})"
            ),
        };
        Self { ownership, message }
    }

    pub fn code(&self) -> &'static str {
        Self::CODE
    }
}

impl std::fmt::Display for SessionMovedAwayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SessionMovedAwayError {}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn header(id: &str) -> Value {
        json!({"type":"session","id":id})
    }
    fn custom(custom_type: &str, data: Value) -> Value {
        json!({"type":"custom","customType":custom_type,"data":data})
    }
    fn refused(state: RefusedState, move_id: &str, target: &str) -> SessionOwnership {
        SessionOwnership::Refused(RefusedOwnership {
            state,
            move_id: move_id.into(),
            target: target.into(),
        })
    }

    #[test]
    fn a_file_without_ownership_entries_is_writable() {
        assert_eq!(
            session_ownership(&[header("s"), json!({"type":"message"})]),
            SessionOwnership::Writable
        );
    }

    #[test]
    fn a_takeover_release_stays_writable_and_a_move_release_is_moving() {
        let takeover = custom(SESSION_RELEASED, json!({"reason":"takeover"}));
        assert_eq!(
            session_ownership(&[header("s"), takeover]),
            SessionOwnership::Writable
        );
        let moving = custom(
            SESSION_RELEASED,
            json!({"reason":"move","move_id":"m1","target":"laptop","session_id":"s"}),
        );
        assert_eq!(
            session_ownership(&[header("s"), moving]),
            refused(RefusedState::Moving, "m1", "laptop")
        );
    }

    #[test]
    fn only_the_same_move_continues_a_moving_session() {
        let moving = custom(
            SESSION_RELEASED,
            json!({"reason":"move","move_id":"m1","target":"t"}),
        );
        let other = custom(SESSION_RECLAIMED, json!({"move_id":"m2"}));
        let reclaimed = custom(SESSION_RECLAIMED, json!({"move_id":"m1"}));
        let moved = custom(SESSION_MOVED, json!({"move_id":"m1","target":"t"}));
        assert_eq!(
            session_ownership(&[header("s"), moving.clone(), other]),
            refused(RefusedState::Moving, "m1", "t")
        );
        assert_eq!(
            session_ownership(&[header("s"), moving.clone(), reclaimed]),
            SessionOwnership::Writable
        );
        assert_eq!(
            session_ownership(&[header("s"), moving, moved]),
            refused(RefusedState::Moved, "m1", "t")
        );
    }

    #[test]
    fn a_moved_session_refuses_everything_after() {
        let moved = custom(SESSION_MOVED, json!({"move_id":"m1"}));
        let reclaimed = custom(SESSION_RECLAIMED, json!({"move_id":"m1"}));
        let verdict = session_ownership(&[header("s"), moved, reclaimed]);
        assert_eq!(verdict, refused(RefusedState::Moved, "m1", ""));
        assert_eq!(
            ownership_step(&verdict, &custom(SESSION_MOVED_IN, json!({})), Some("s")),
            OwnershipStep::Refused
        );
    }

    #[test]
    fn entries_naming_another_session_are_ignored() {
        let inherited = custom(
            SESSION_RELEASED,
            json!({"reason":"move","move_id":"m1","session_id":"parent"}),
        );
        assert_eq!(
            session_ownership(&[header("child"), inherited]),
            SessionOwnership::Writable
        );
    }

    #[test]
    fn the_error_names_the_state_and_target() {
        let moving = SessionMovedAwayError::new(
            "/f",
            RefusedOwnership {
                state: RefusedState::Moving,
                move_id: "m".into(),
                target: String::new(),
            },
        );
        assert_eq!(moving.code(), "session_moved_away");
        assert!(
            moving
                .to_string()
                .starts_with("This session is being moved to another device;")
        );
        let moved = SessionMovedAwayError::new(
            "/f",
            RefusedOwnership {
                state: RefusedState::Moved,
                move_id: "m".into(),
                target: "laptop".into(),
            },
        );
        assert!(
            moved
                .to_string()
                .ends_with("Open it on laptop, or make a copy here. (/f)")
        );
    }
}
