//! Port of senpi `packages/coding-agent/src/modes/rpc/permission-preset-command.ts`.
//!
//! The live setter a classic `--mode rpc` session has no other way to receive (it refuses
//! `open_session`), and the path-free one for a session on another device. An unknown preset is
//! refused before anything changes, so the session keeps the mode it had; a valid one is applied
//! through the runtime the way an attach naming it applies it.

use crate::rpc_types::{
    RPC_ERROR_INVALID_LAUNCH_PROFILE, ResponseRecordType, RpcResponse, RpcResponseResult,
};
use futures::future::BoxFuture;
use maho_core::agent_session_runtime::AgentSessionRuntime;
use maho_core::permission_decision::PermissionPresetName;
use maho_core::permission_mode::{
    PermissionMode, PermissionModeReply, PermissionPresetSource, PermissionPresetStatus,
    client_mode, permission_mode_reply,
};

/// Minimal runtime surface needed by `run_set_permission_preset`.
pub trait PermissionPresetRuntime {
    fn set_permission_preset<'a>(&'a mut self, preset: String) -> BoxFuture<'a, ()>;
    fn current_permission_mode(&self) -> PermissionMode;
}

impl PermissionPresetRuntime for AgentSessionRuntime {
    fn set_permission_preset<'a>(&'a mut self, preset: String) -> BoxFuture<'a, ()> {
        Box::pin(AgentSessionRuntime::set_permission_preset(self, preset))
    }

    fn current_permission_mode(&self) -> PermissionMode {
        let session_id = self.session().session_id();
        let active = maho_core::permission_mode::client_preset_for_session(&session_id)
            .and_then(|value| PermissionPresetName::parse(&value))
            .unwrap_or(PermissionPresetName::Guarded);
        let enforced = maho_core::permission_decision::decision_preset_for_session(&session_id);
        client_mode(active, enforced)
    }
}

pub async fn run_set_permission_preset<R: PermissionPresetRuntime + ?Sized>(
    runtime: &mut R,
    id: Option<String>,
    permission_preset: &str,
) -> RpcResponse {
    if PermissionPresetName::parse(permission_preset).is_none() {
        return RpcResponse {
            id,
            record_type: ResponseRecordType::Response,
            command: "set_permission_preset".to_owned(),
            session_id: None,
            result: RpcResponseResult::Error {
                error: format!(
                    "{RPC_ERROR_INVALID_LAUNCH_PROFILE}: unknown permissionPreset \"{permission_preset}\""
                ),
                error_code: Some(RPC_ERROR_INVALID_LAUNCH_PROFILE.to_owned()),
                error_data: None,
            },
        };
    }
    runtime
        .set_permission_preset(permission_preset.to_owned())
        .await;
    let reply: PermissionModeReply = permission_mode_reply(&runtime.current_permission_mode());
    let data = serde_json::to_value(reply).ok();
    RpcResponse {
        id,
        record_type: ResponseRecordType::Response,
        command: "set_permission_preset".to_owned(),
        session_id: None,
        result: RpcResponseResult::Success { data },
    }
}

/// Builds a `PermissionModeReply` directly from a preset string when the caller only needs the
/// validated client reply shape.
pub fn client_permission_mode_reply(preset: &str) -> Option<PermissionModeReply> {
    let parsed = PermissionPresetName::parse(preset)?;
    Some(PermissionModeReply {
        permission_preset: parsed.as_str().to_owned(),
        permission_preset_source: PermissionPresetSource::Client,
        permission_preset_status: PermissionPresetStatus::Applied,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeRuntime {
        preset: String,
    }

    impl PermissionPresetRuntime for FakeRuntime {
        fn set_permission_preset<'a>(&'a mut self, preset: String) -> BoxFuture<'a, ()> {
            Box::pin(async move {
                self.preset = preset;
            })
        }

        fn current_permission_mode(&self) -> PermissionMode {
            let parsed =
                PermissionPresetName::parse(&self.preset).unwrap_or(PermissionPresetName::Guarded);
            client_mode(parsed, None)
        }
    }

    #[tokio::test]
    async fn unknown_preset_refuses_without_mutating_runtime() {
        let mut rt = FakeRuntime {
            preset: "ask".into(),
        };
        let resp = run_set_permission_preset(&mut rt, Some("1".into()), "not-a-preset").await;
        assert_eq!(rt.preset, "ask");
        match resp.result {
            RpcResponseResult::Error { error_code, .. } => {
                assert_eq!(
                    error_code.as_deref(),
                    Some(RPC_ERROR_INVALID_LAUNCH_PROFILE)
                );
            }
            RpcResponseResult::Success { .. } => panic!("expected error"),
        }
    }

    #[tokio::test]
    async fn valid_preset_updates_runtime_and_returns_mode_reply() {
        let mut rt = FakeRuntime {
            preset: "ask".into(),
        };
        let resp = run_set_permission_preset(&mut rt, Some("2".into()), "guarded").await;
        assert_eq!(rt.preset, "guarded");
        match resp.result {
            RpcResponseResult::Success { data } => {
                let data = data.expect("reply data");
                assert_eq!(data["permissionPreset"], "guarded");
                assert_eq!(data["permissionPresetSource"], "client");
                assert_eq!(data["permissionPresetStatus"], "applied");
            }
            RpcResponseResult::Error { error, .. } => panic!("unexpected error: {error}"),
        }
    }
}
