//! Read views over the boulder document: the state root, its works and task sessions.
//!
//! Each view wraps the raw JSON object rather than a closed struct so unknown fields
//! (`worktree_note`, `completed_note`, ...) and `null`s survive a read/write cycle byte
//! for byte, exactly as they do through the TypeScript spread-based updates.

use serde_json::Value;

use crate::js::{Js, JsObj};
use crate::types::{BoulderSessionOrigin, BoulderTaskStatus, BoulderWorkStatus};

macro_rules! string_fields {
    ($($name:ident),* $(,)?) => {
        $(
            pub fn $name(&self) -> Option<&str> {
                self.doc.get_str(stringify!($name))
            }
        )*
    };
}

macro_rules! work_fields {
    () => {
        string_fields!(
            active_plan,
            plan_name,
            started_at,
            ended_at,
            updated_at,
            // Stamped when a stale-work reconcile demotes the work to `paused`.
            stale_since,
            agent,
            worktree_path
        );

        pub fn elapsed_ms(&self) -> Option<i64> {
            self.doc.get("elapsed_ms").and_then(Js::as_i64)
        }

        pub fn status(&self) -> Option<BoulderWorkStatus> {
            self.doc
                .get_str("status")
                .and_then(BoulderWorkStatus::parse)
        }

        /// String entries of `session_ids`, in order.
        pub fn session_ids(&self) -> Vec<String> {
            crate::records::string_items(&self.doc, "session_ids")
        }

        pub fn session_origin(&self, session_id: &str) -> Option<BoulderSessionOrigin> {
            self.doc
                .get_obj("session_origins")
                .and_then(|origins| origins.get_str(session_id))
                .and_then(BoulderSessionOrigin::parse)
        }

        pub fn task_session(&self, task_key: &str) -> Option<TaskSessionState> {
            self.doc
                .get_obj("task_sessions")
                .and_then(|sessions| sessions.get_obj(task_key))
                .cloned()
                .map(TaskSessionState::from_doc)
        }

        /// The object as `JSON.stringify` would emit it.
        pub fn to_json_value(&self) -> Value {
            Value::Object(self.doc.to_map())
        }
    };
}

/// A `boulder.json` document (schema v2 root with its active-work mirror).
#[derive(Clone, Debug, PartialEq)]
pub struct BoulderState {
    pub(crate) doc: JsObj,
}

impl BoulderState {
    pub(crate) fn from_doc(doc: JsObj) -> Self {
        Self { doc }
    }

    /// Wrap a JSON object as state; `None` when the value is not an object.
    pub fn from_json_value(value: Value) -> Option<Self> {
        match Js::from_value(value) {
            Js::Object(doc) => Some(Self { doc }),
            Js::Undefined | Js::Value(_) => None,
        }
    }

    /// `JSON.stringify(state, null, 2)`: the exact bytes the writer persists.
    pub fn to_json_string_pretty(&self) -> String {
        self.doc.stringify_pretty()
    }

    pub fn schema_version(&self) -> Option<i64> {
        self.doc.get("schema_version").and_then(Js::as_i64)
    }

    string_fields!(active_work_id);

    /// Entry of the `works` map stored under `key`.
    pub fn work(&self, key: &str) -> Option<BoulderWorkState> {
        self.doc
            .get_obj("works")
            .and_then(|works| works.get_obj(key))
            .cloned()
            .map(BoulderWorkState::from_doc)
    }

    work_fields!();
}

/// One work of the `works` map (or the legacy work synthesised from the mirror).
#[derive(Clone, Debug, PartialEq)]
pub struct BoulderWorkState {
    pub(crate) doc: JsObj,
}

impl BoulderWorkState {
    pub(crate) fn from_doc(doc: JsObj) -> Self {
        Self { doc }
    }

    string_fields!(work_id);

    work_fields!();
}

/// Per-task session record under `task_sessions`.
#[derive(Clone, Debug, PartialEq)]
pub struct TaskSessionState {
    pub(crate) doc: JsObj,
}

impl TaskSessionState {
    pub(crate) fn from_doc(doc: JsObj) -> Self {
        Self { doc }
    }

    string_fields!(
        task_key, task_label, task_title, session_id, agent, category, started_at, ended_at,
        updated_at
    );

    pub fn elapsed_ms(&self) -> Option<i64> {
        self.doc.get("elapsed_ms").and_then(Js::as_i64)
    }

    pub fn status(&self) -> Option<BoulderTaskStatus> {
        self.doc
            .get_str("status")
            .and_then(BoulderTaskStatus::parse)
    }

    pub fn to_json_value(&self) -> Value {
        Value::Object(self.doc.to_map())
    }
}

pub(crate) fn string_items(doc: &JsObj, key: &str) -> Vec<String> {
    doc.get(key)
        .and_then(Js::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}
