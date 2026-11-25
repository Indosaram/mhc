//! Port of senpi `packages/coding-agent/src/modes/rpc/socket-secret-publish.ts`.
//!
//! First publication of the public socket secret through the Windows bridge. The bridge result
//! crosses a native boundary, so it is parsed here, never trusted.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A staging file whose delete disposition failed; it is still at `path` and the caller removes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeLeftBehindStaging {
    pub path: String,
    pub windows_error: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativePublishFailure {
    pub step: String,
    pub code: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativePublishResult {
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<NativePublishFailure>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub staging_left_behind: Option<NativeLeftBehindStaging>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeftBehindStaging {
    pub path: String,
    pub windows_error: i64,
    /// What the caller must do; names the path, never the secret.
    pub action: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketSecretOutcome {
    Published,
    Collision,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SocketSecretPublication {
    pub outcome: SocketSecretOutcome,
    pub staging_left_behind: Option<LeftBehindStaging>,
}

/// The bridge could not publish; `staging_left_behind` is set when its staging file also survived.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct SocketSecretPublishError {
    pub message: String,
    pub step: String,
    pub windows_error: Option<i64>,
    pub staging_left_behind: Option<LeftBehindStaging>,
}

impl SocketSecretPublishError {
    pub const CODE: &'static str = "socket-secret-publish-failed";

    pub fn new(
        step: impl Into<String>,
        windows_error: Option<i64>,
        staging_left_behind: Option<LeftBehindStaging>,
    ) -> Self {
        let step = step.into();
        let cause = match windows_error {
            None => step.clone(),
            Some(code) => format!("{step} failed with Windows error {code}"),
        };
        let leftover = match &staging_left_behind {
            Some(left) => format!("; {}", left.action),
            None => String::new(),
        };
        let message = format!("Publishing the socket secret failed: {cause}{leftover}");
        Self {
            message,
            step,
            windows_error,
            staging_left_behind,
        }
    }
}

pub trait SocketSecretPublishBinding {
    fn publish_no_replace(
        &self,
        staging_path: &str,
        final_path: &str,
        bytes: &[u8],
    ) -> Result<Value, String>;
}

/// First publication through the bridge (design 4.3).
pub fn publish_socket_secret_no_replace(
    binding: &dyn SocketSecretPublishBinding,
    staging_path: &str,
    final_path: &str,
    bytes: &[u8],
) -> Result<SocketSecretPublication, SocketSecretPublishError> {
    let result = match binding.publish_no_replace(staging_path, final_path, bytes) {
        Ok(val) => val,
        Err(reason) => {
            return Err(SocketSecretPublishError::new(
                format!("bridge call threw: {reason}"),
                None,
                None,
            ));
        }
    };
    let Some(obj) = result.as_object() else {
        return Err(SocketSecretPublishError::new(
            format!("unexpected bridge result {result}"),
            None,
            None,
        ));
    };
    let staging_left_behind = match obj.get("stagingLeftBehind") {
        None | Some(Value::Null) => None,
        Some(raw) => {
            let Some(left_obj) = raw.as_object() else {
                return Err(SocketSecretPublishError::new(
                    "unexpected bridge result: malformed stagingLeftBehind",
                    None,
                    None,
                ));
            };
            let (Some(path), Some(windows_error)) = (
                left_obj.get("path").and_then(Value::as_str),
                left_obj.get("windowsError").and_then(Value::as_i64),
            ) else {
                return Err(SocketSecretPublishError::new(
                    "unexpected bridge result: malformed stagingLeftBehind",
                    None,
                    None,
                ));
            };
            Some(LeftBehindStaging {
                path: path.to_owned(),
                windows_error,
                action: format!("Delete the leftover socket-secret staging file {path}"),
            })
        }
    };
    match obj.get("outcome").and_then(Value::as_str) {
        Some("published") => Ok(SocketSecretPublication {
            outcome: SocketSecretOutcome::Published,
            staging_left_behind,
        }),
        Some("collision") => Ok(SocketSecretPublication {
            outcome: SocketSecretOutcome::Collision,
            staging_left_behind,
        }),
        Some("failed") => {
            let Some(failure) = obj.get("failure").and_then(Value::as_object) else {
                return Err(SocketSecretPublishError::new(
                    "unexpected bridge result: malformed failure",
                    None,
                    staging_left_behind,
                ));
            };
            let (Some(step), Some(code)) = (
                failure.get("step").and_then(Value::as_str),
                failure.get("code").and_then(Value::as_i64),
            ) else {
                return Err(SocketSecretPublishError::new(
                    "unexpected bridge result: malformed failure",
                    None,
                    staging_left_behind,
                ));
            };
            Err(SocketSecretPublishError::new(
                step,
                Some(code),
                staging_left_behind,
            ))
        }
        other => Err(SocketSecretPublishError::new(
            format!("unexpected bridge outcome {:?}", other),
            None,
            staging_left_behind,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct FakeBinding(Result<Value, String>);
    impl SocketSecretPublishBinding for FakeBinding {
        fn publish_no_replace(&self, _: &str, _: &str, _: &[u8]) -> Result<Value, String> {
            self.0.clone()
        }
    }

    #[test]
    fn parses_published_collision_and_typed_failures() {
        let ok = publish_socket_secret_no_replace(
            &FakeBinding(Ok(json!({ "outcome": "published" }))),
            "s",
            "f",
            b"secret",
        )
        .expect("published");
        assert_eq!(ok.outcome, SocketSecretOutcome::Published);
        assert!(ok.staging_left_behind.is_none());

        let collision = publish_socket_secret_no_replace(
            &FakeBinding(Ok(json!({
                "outcome": "collision",
                "stagingLeftBehind": { "path": "C:\\tmp\\s", "windowsError": 5 }
            }))),
            "s",
            "f",
            b"secret",
        )
        .expect("collision");
        assert_eq!(collision.outcome, SocketSecretOutcome::Collision);
        assert_eq!(
            collision
                .staging_left_behind
                .expect("leftover")
                .windows_error,
            5
        );

        let err = publish_socket_secret_no_replace(
            &FakeBinding(Ok(json!({
                "outcome": "failed",
                "failure": { "step": "link", "code": 32 }
            }))),
            "s",
            "f",
            b"secret",
        )
        .unwrap_err();
        assert_eq!(err.step, "link");
        assert_eq!(err.windows_error, Some(32));
    }
}
