//! Port of senpi packages/agent/src/harness/result.ts.
//!
//! TS models a fallible outcome as the discriminated union
//! `{ ok: true; value } | { ok: false; error }`. Rust's `Result` is the same shape, so the
//! `Result` type alias, `ok`, `err`, `isOk` and `isErr` are the std equivalents; the helpers
//! below keep the TS names for a mechanical diff. `getOrThrow` throws in TS, which Rust models
//! as a panic (it is documented as "for tests and explicit adapter boundaries").
//!
//! `TaggedError(tag)` produces a class with a `_tag` field and a `toJSON` that emits
//! `{ _tag, message, ...ownProps }`. Rust has no runtime class factory, so every tag becomes a
//! concrete struct implementing [`TaggedErrorValue`], whose [`TaggedErrorValue::to_json`]
//! reproduces `toJSON` exactly (own enumerable props only, `_tag` first, `message` second).

use std::error::Error;
use std::fmt;

use serde_json::{Map, Value, json};

/// `Result<TValue, TError>`. The TS union is structurally std's `Result`.
pub type Result<TValue, TError> = std::result::Result<TValue, TError>;

/// `Result.ok(value)`.
pub fn ok<TValue, TError>(value: TValue) -> Result<TValue, TError> {
    Ok(value)
}

/// `Result.err(error)`.
pub fn err<TValue, TError>(error: TError) -> Result<TValue, TError> {
    Err(error)
}

/// `Result.isOk(result)`.
pub fn is_ok<TValue, TError>(result: &Result<TValue, TError>) -> bool {
    result.is_ok()
}

/// `Result.isErr(result)`.
pub fn is_err<TValue, TError>(result: &Result<TValue, TError>) -> bool {
    result.is_err()
}

/// `getOrThrow(result)`: return the success value or throw the failure error.
///
/// TS throws the typed error value itself; Rust panics with its `Display` text at the explicit
/// adapter boundaries the TS doc calls out.
pub fn get_or_throw<TValue, TError: fmt::Display>(result: Result<TValue, TError>) -> TValue {
    match result {
        Ok(value) => value,
        Err(error) => panic!("{error}"),
    }
}

/// One `TaggedError(tag)` class: a `_tag`, a `message`, and `toJSON()`.
pub trait TaggedErrorValue: Error + Send + Sync + 'static {
    /// The `_tag` discriminant.
    const TAG: &'static str;

    /// `this._tag`.
    fn tag(&self) -> &'static str {
        Self::TAG
    }

    /// `toJSON()`: `{ _tag, message, ...ownProps }`.
    fn to_json(&self) -> Value;
}

/// `OperationKind` as it appears in `LaneBusy.operationKind` and `CurrentOperationInfo.kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OperationKind {
    Run,
    Compaction,
    Navigation,
}

impl OperationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            OperationKind::Run => "run",
            OperationKind::Compaction => "compaction",
            OperationKind::Navigation => "navigation",
        }
    }
}

macro_rules! tagged_error {
    ($(#[$meta:meta])* $name:ident, $tag:literal, { $($field:ident : $ty:ty),* $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug)]
        pub struct $name {
            $(pub $field: $ty,)*
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.message)
            }
        }

        impl Error for $name {}

        impl TaggedErrorValue for $name {
            const TAG: &'static str = $tag;

            fn to_json(&self) -> Value {
                let mut map = Map::new();
                map.insert("_tag".to_string(), json!($tag));
                map.insert("message".to_string(), json!(self.message));
                $(map.insert(stringify!($field).to_string(), json!(self.$field));)*
                Value::Object(map)
            }
        }
    };
}

tagged_error!(
    /// `LaneBusy`: the lane already has an active operation of the requested kind.
    LaneBusy,
    "LaneBusy",
    {
        lane: String,
        operation_id: String,
        operation_kind: OperationKind,
        message: String,
    }
);

tagged_error!(
    /// `OperationMismatch`: the addressed operation is not the lane's current one.
    OperationMismatch,
    "OperationMismatch",
    {
        lane: String,
        expected_operation_id: String,
        current_operation_id: Option<String>,
        last_operation_id: Option<String>,
        message: String,
    }
);

tagged_error!(
    /// `NoActiveRun`.
    NoActiveRun,
    "NoActiveRun",
    {
        lane: String,
        message: String,
    }
);

tagged_error!(
    /// `NoActiveOperation`.
    NoActiveOperation,
    "NoActiveOperation",
    {
        lane: String,
        message: String,
    }
);

tagged_error!(
    /// `NothingToResume`.
    NothingToResume,
    "NothingToResume",
    {
        lane: String,
        message: String,
    }
);

tagged_error!(
    /// `NothingToCompact`.
    NothingToCompact,
    "NothingToCompact",
    {
        lane: String,
        message: String,
    }
);

tagged_error!(
    /// `InvalidMessage`.
    InvalidMessage,
    "InvalidMessage",
    {
        lane: String,
        reason: String,
        message: String,
    }
);

tagged_error!(
    /// `InvalidNavigation`.
    InvalidNavigation,
    "InvalidNavigation",
    {
        lane: String,
        reason: String,
        message: String,
    }
);

tagged_error!(
    /// `UnknownSkill`.
    UnknownSkill,
    "UnknownSkill",
    {
        name: String,
        message: String,
    }
);

tagged_error!(
    /// `UnknownTemplate`.
    UnknownTemplate,
    "UnknownTemplate",
    {
        name: String,
        message: String,
    }
);

tagged_error!(
    /// `UnknownTarget`.
    UnknownTarget,
    "UnknownTarget",
    {
        target_id: String,
        message: String,
    }
);

tagged_error!(
    /// `InvalidLane`.
    InvalidLane,
    "InvalidLane",
    {
        lane: String,
        reason: String,
        message: String,
    }
);

tagged_error!(
    /// `Closed`.
    Closed,
    "Closed",
    {
        message: String,
    }
);

/// `HarnessFault(message, cause)`: an unexpected failure wrapped with its cause.
#[derive(Debug)]
pub struct HarnessFault {
    pub message: String,
    pub cause: Option<Box<dyn Error + Send + Sync + 'static>>,
}

impl HarnessFault {
    pub fn new(message: impl Into<String>, cause: Option<Box<dyn Error + Send + Sync + 'static>>) -> Self {
        Self { message: message.into(), cause }
    }
}

impl fmt::Display for HarnessFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for HarnessFault {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.cause.as_deref().map(|cause| cause as &(dyn Error + 'static))
    }
}

/// `HarnessClosed`: `AgentHarness was closed while the operation was active`.
#[derive(Debug)]
pub struct HarnessClosed;

impl HarnessClosed {
    pub const MESSAGE: &'static str = "AgentHarness was closed while the operation was active";

    pub fn new() -> Self {
        Self
    }
}

impl Default for HarnessClosed {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for HarnessClosed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(Self::MESSAGE)
    }
}

impl Error for HarnessClosed {}

/// `matchError(error, matchers)`.
pub type ErrorMatcher<TError, TValue> = fn(&TError) -> TValue;

/// `matchError(error, matchers)`: dispatch on `error._tag` and call the matching matcher.
///
/// TS's matcher table is an object keyed by tag with a statically exhaustive type; Rust callers
/// pass the matcher for the tag they expect, mirroring the TS call sites that reach for one arm.
pub fn match_error<TError, TValue>(
    error: &TError,
    matchers: &[(&str, ErrorMatcher<TError, TValue>)],
) -> Option<TValue>
where
    TError: TaggedErrorValue,
{
    matchers
        .iter()
        .find(|(tag, _)| *tag == error.tag())
        .map(|(_, matcher)| matcher(error))
}
