//! Scripted faux-provider turns shared with `tools/golden/faux-harness.mjs`.
//!
//! A script (`tools/golden/scripts/<name>.json`) is a prompt plus queued assistant responses.
//! [`FauxQueue`] hands them out in order and fails like senpi's faux provider once empty.

use std::collections::VecDeque;
use std::fmt;
use std::path::PathBuf;

use serde::Deserialize;

use crate::golden::workspace_root;

/// Error text senpi's faux provider emits when no response is queued.
pub const NO_MORE_RESPONSES: &str = "No more faux responses queued";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FauxScript {
    pub name: String,
    pub prompt: String,
    pub responses: Vec<FauxResponse>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FauxResponse {
    pub content: String,
    #[serde(default = "default_stop_reason")]
    pub stop_reason: String,
}

fn default_stop_reason() -> String {
    "stop".to_string()
}

#[derive(Debug)]
pub enum FauxError {
    InvalidName(String),
    Read { path: PathBuf, source: std::io::Error },
    Parse { path: PathBuf, source: serde_json::Error },
    Exhausted,
}

impl fmt::Display for FauxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidName(name) => write!(f, "invalid faux script name {name:?}"),
            Self::Read { path, source } => write!(f, "cannot read {}: {source}", path.display()),
            Self::Parse { path, source } => write!(f, "cannot parse {}: {source}", path.display()),
            Self::Exhausted => f.write_str(NO_MORE_RESPONSES),
        }
    }
}

impl std::error::Error for FauxError {}

/// Path of a named script under `tools/golden/scripts`.
pub fn script_path(name: &str) -> Result<PathBuf, FauxError> {
    let valid = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.starts_with('-');
    if !valid {
        return Err(FauxError::InvalidName(name.to_string()));
    }
    Ok(workspace_root()
        .join("tools")
        .join("golden")
        .join("scripts")
        .join(format!("{name}.json")))
}

pub fn load_script(name: &str) -> Result<FauxScript, FauxError> {
    let path = script_path(name)?;
    let text = std::fs::read_to_string(&path).map_err(|source| FauxError::Read {
        path: path.clone(),
        source,
    })?;
    serde_json::from_str(&text).map_err(|source| FauxError::Parse { path, source })
}

/// Ordered queue of scripted responses.
#[derive(Debug, Clone, Default)]
pub struct FauxQueue {
    pending: VecDeque<FauxResponse>,
    calls: usize,
}

impl FauxQueue {
    pub fn new(responses: impl IntoIterator<Item = FauxResponse>) -> Self {
        Self {
            pending: responses.into_iter().collect(),
            calls: 0,
        }
    }

    pub fn from_script(script: &FauxScript) -> Self {
        Self::new(script.responses.iter().cloned())
    }

    pub fn append(&mut self, responses: impl IntoIterator<Item = FauxResponse>) {
        self.pending.extend(responses);
    }

    /// Takes the next response; counts the call even when the queue is empty, as senpi does.
    pub fn next_response(&mut self) -> Result<FauxResponse, FauxError> {
        self.calls += 1;
        self.pending.pop_front().ok_or(FauxError::Exhausted)
    }

    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    pub fn call_count(&self) -> usize {
        self.calls
    }
}
