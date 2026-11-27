//! Port of senpi `packages/agent/src/execution/device-files.ts`.

use std::collections::HashMap;
use std::sync::Arc;

use base64::Engine;
use maho_ai::types::BoxFuture;
use maho_ai::utils::abort::AbortSignal;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The channel's name in a hand's `hello`.
pub const FILE_CHANNEL: &str = "file/1";

/// The most data one operation reads or writes, in raw bytes, in both directions.
pub const FILE_CHANNEL_MAX_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeviceFileOp {
    Read,
    Write,
    List,
    Stat,
    Rename,
    Remove,
    Mkdir,
    Run,
}

impl DeviceFileOp {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::List => "list",
            Self::Stat => "stat",
            Self::Rename => "rename",
            Self::Remove => "remove",
            Self::Mkdir => "mkdir",
            Self::Run => "run",
        }
    }
}

pub const DEVICE_FILE_OPS: [DeviceFileOp; 8] = [
    DeviceFileOp::Read,
    DeviceFileOp::Write,
    DeviceFileOp::List,
    DeviceFileOp::Stat,
    DeviceFileOp::Rename,
    DeviceFileOp::Remove,
    DeviceFileOp::Mkdir,
    DeviceFileOp::Run,
];

/// The `run` operation's name in a hand's `hello`.
pub const RUN_CHANNEL: &str = "run/1";

#[derive(Clone, Default)]
pub struct DeviceFileOptions {
    pub signal: Option<AbortSignal>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DeviceFileWriteOptions {
    pub exclusive: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeviceFileKind {
    File,
    Directory,
    Other,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceFileStat {
    pub kind: DeviceFileKind,
    pub size: u64,
    pub mtime_ms: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceRunRequest {
    pub exe: String,
    pub argv: Vec<String>,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdin: Option<Vec<u8>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity_lock: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceRunResult {
    pub code: Option<i32>,
    pub signal: Option<String>,
    pub stdout: Vec<u8>,
    pub stderr: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceFileErrorCode {
    NotFound,
    NotAFile,
    Exists,
    TooLarge,
    Io,
    Aborted,
    RefusedByGrant,
    RefusedByPolicy,
    HandUnreachable,
    Unsupported,
    Interrupted,
    Protocol,
}

impl DeviceFileErrorCode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NotFound => "not_found",
            Self::NotAFile => "not_a_file",
            Self::Exists => "exists",
            Self::TooLarge => "too_large",
            Self::Io => "io",
            Self::Aborted => "aborted",
            Self::RefusedByGrant => "refused_by_grant",
            Self::RefusedByPolicy => "refused_by_policy",
            Self::HandUnreachable => "hand_unreachable",
            Self::Unsupported => "unsupported",
            Self::Interrupted => "interrupted",
            Self::Protocol => "protocol",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "not_found" => Some(Self::NotFound),
            "not_a_file" => Some(Self::NotAFile),
            "exists" => Some(Self::Exists),
            "too_large" => Some(Self::TooLarge),
            "io" => Some(Self::Io),
            "aborted" => Some(Self::Aborted),
            "refused_by_grant" => Some(Self::RefusedByGrant),
            "refused_by_policy" => Some(Self::RefusedByPolicy),
            "hand_unreachable" => Some(Self::HandUnreachable),
            "unsupported" => Some(Self::Unsupported),
            "interrupted" => Some(Self::Interrupted),
            "protocol" => Some(Self::Protocol),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{code_str}: {message}", code_str = .code.as_str())]
pub struct DeviceFileError {
    pub code: DeviceFileErrorCode,
    pub message: String,
    pub reason: Option<String>,
    pub uncertain: bool,
}

impl DeviceFileError {
    pub fn new(code: DeviceFileErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            reason: None,
            uncertain: false,
        }
    }

    pub fn with_detail(
        code: DeviceFileErrorCode,
        message: impl Into<String>,
        reason: Option<String>,
        uncertain: bool,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            reason,
            uncertain,
        }
    }
}

pub trait DeviceFiles: Send + Sync {
    fn read(
        &self,
        path: &str,
        options: DeviceFileOptions,
    ) -> BoxFuture<'_, Result<Vec<u8>, DeviceFileError>>;

    fn write(
        &self,
        path: &str,
        data: Vec<u8>,
        options: DeviceFileOptions,
        write_options: DeviceFileWriteOptions,
    ) -> BoxFuture<'_, Result<(), DeviceFileError>>;

    fn list(
        &self,
        path: &str,
        options: DeviceFileOptions,
    ) -> BoxFuture<'_, Result<Vec<String>, DeviceFileError>>;

    fn stat(
        &self,
        path: &str,
        options: DeviceFileOptions,
    ) -> BoxFuture<'_, Result<DeviceFileStat, DeviceFileError>>;

    fn rename(
        &self,
        from: &str,
        to: &str,
        options: DeviceFileOptions,
    ) -> BoxFuture<'_, Result<(), DeviceFileError>>;

    fn remove(
        &self,
        path: &str,
        options: DeviceFileOptions,
    ) -> BoxFuture<'_, Result<(), DeviceFileError>>;

    fn mkdir(
        &self,
        path: &str,
        options: DeviceFileOptions,
    ) -> BoxFuture<'_, Result<(), DeviceFileError>>;

    fn run(
        &self,
        request: DeviceRunRequest,
        options: DeviceFileOptions,
    ) -> BoxFuture<'_, Result<DeviceRunResult, DeviceFileError>>;

    fn is_local(&self) -> bool {
        false
    }
}

pub fn is_local_device_files(files: &dyn DeviceFiles) -> bool {
    files.is_local()
}

pub fn is_local_device_files_arc(files: &Arc<dyn DeviceFiles>) -> bool {
    files.is_local()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum DeviceFileArgs {
    Read {
        path: String,
    },
    List {
        path: String,
    },
    Stat {
        path: String,
    },
    Remove {
        path: String,
    },
    Mkdir {
        path: String,
    },
    Write {
        path: String,
        data: String,
        exclusive: bool,
    },
    Rename {
        path: String,
        to: String,
    },
    Run {
        path: String,
        exe: String,
        argv: Vec<String>,
        env: HashMap<String, String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stdin: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
        #[serde(rename = "activityLock", default)]
        activity_lock: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DeviceFileOutcome {
    Ok {
        ok: bool,
        value: Value,
    },
    Err {
        ok: bool,
        code: DeviceFileErrorCode,
        message: String,
    },
}

impl DeviceFileOutcome {
    pub fn ok(value: Value) -> Self {
        Self::Ok { ok: true, value }
    }

    pub fn err(code: DeviceFileErrorCode, message: impl Into<String>) -> Self {
        Self::Err {
            ok: false,
            code,
            message: message.into(),
        }
    }
}

pub fn decode_device_file_args(op: &str, args: &Value) -> Option<DeviceFileArgs> {
    let map = args.as_object()?;
    let path = map.get("path")?.as_str()?;
    if path.is_empty() {
        return None;
    }
    match op {
        "read" => Some(DeviceFileArgs::Read {
            path: path.to_owned(),
        }),
        "list" => Some(DeviceFileArgs::List {
            path: path.to_owned(),
        }),
        "stat" => Some(DeviceFileArgs::Stat {
            path: path.to_owned(),
        }),
        "remove" => Some(DeviceFileArgs::Remove {
            path: path.to_owned(),
        }),
        "mkdir" => Some(DeviceFileArgs::Mkdir {
            path: path.to_owned(),
        }),
        "write" => {
            let data = map.get("data")?.as_str()?.to_owned();
            let exclusive = map
                .get("exclusive")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            Some(DeviceFileArgs::Write {
                path: path.to_owned(),
                data,
                exclusive,
            })
        }
        "rename" => {
            let to = map.get("to")?.as_str()?;
            if to.is_empty() {
                return None;
            }
            Some(DeviceFileArgs::Rename {
                path: path.to_owned(),
                to: to.to_owned(),
            })
        }
        "run" => decode_run_args(path, map),
        _ => None,
    }
}

fn decode_run_args(path: &str, map: &serde_json::Map<String, Value>) -> Option<DeviceFileArgs> {
    let exe = map.get("exe")?.as_str()?.to_owned();
    let argv_vals = map.get("argv")?.as_array()?;
    let mut argv = Vec::with_capacity(argv_vals.len());
    for item in argv_vals {
        argv.push(item.as_str()?.to_owned());
    }
    let env = match map.get("env") {
        None | Some(Value::Null) => HashMap::new(),
        Some(val) => {
            let obj = val.as_object()?;
            let mut out = HashMap::with_capacity(obj.len());
            for (k, v) in obj {
                out.insert(k.clone(), v.as_str()?.to_owned());
            }
            out
        }
    };
    let stdin = match map.get("stdin") {
        None => None,
        Some(v) => Some(v.as_str()?.to_owned()),
    };
    let timeout_ms = match map.get("timeoutMs") {
        None => None,
        Some(v) => {
            let n = v.as_u64()?;
            if n == 0 {
                return None;
            }
            Some(n)
        }
    };
    let activity_lock = match map.get("activityLock") {
        None => false,
        Some(v) => v.as_bool()?,
    };
    Some(DeviceFileArgs::Run {
        path: path.to_owned(),
        exe,
        argv,
        env,
        stdin,
        timeout_ms,
        activity_lock,
    })
}

pub fn encode_base64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

pub fn decode_base64(text: &str) -> Result<Vec<u8>, DeviceFileError> {
    base64::engine::general_purpose::STANDARD
        .decode(text)
        .map_err(|e| DeviceFileError::new(DeviceFileErrorCode::Protocol, e.to_string()))
}

pub fn base64_length(text: &str) -> usize {
    let padding = if text.ends_with("==") {
        2
    } else if text.ends_with('=') {
        1
    } else {
        0
    };
    (text.len() * 3) / 4 - padding
}

pub fn file_outcome_of(details: &Value) -> Option<DeviceFileOutcome> {
    let file = details.as_object()?.get("file")?.as_object()?;
    let ok = file.get("ok")?.as_bool()?;
    if ok {
        let value = file.get("value")?.clone();
        Some(DeviceFileOutcome::ok(value))
    } else {
        let code_str = file.get("code")?.as_str()?;
        let code = DeviceFileErrorCode::parse(code_str)?;
        let message = file.get("message")?.as_str()?.to_owned();
        Some(DeviceFileOutcome::err(code, message))
    }
}
