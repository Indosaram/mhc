//! Port of senpi `packages/coding-agent/src/modes/rpc/user-skill-approval.ts`.
//!
//! The approval half of `write_user_skill_file`: a write from another device commits only once
//! someone on THIS device approves it through the session's local grant presentation, which only
//! an attached local client registers (never a remote or delegated connection), so the requester
//! cannot answer its own question. There is no "always allow" choice. Every such attempt leaves
//! one local audit line.

use crate::project_write_gate::ProjectWriteRefusal;
use futures::future::BoxFuture;
use maho_ai::utils::abort::{AbortController, AbortSignal};
use maho_core::agent_session::AgentSession;
use maho_tools::unified_diff::create_unified_patch;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

pub const RPC_ERROR_USER_SKILL_WRITE_DECLINED: &str = "user_skill_write_declined";
pub const RPC_ERROR_USER_SKILL_WRITE_UNCONFIRMED: &str = "user_skill_write_unconfirmed";

pub const USER_SKILL_ALLOW: &str = "Allow this change";
pub const USER_SKILL_DENY: &str = "Deny";
pub const USER_SKILL_AUDIT_FILE: &str = "audit/user-skill-writes.jsonl";

/// A skill file may be 512 KiB; a longer diff is cut and the prompt says so.
const PROMPT_DIFF_MAX_CHARACTERS: usize = 20_000;
const LOCAL_GRANT_TIMEOUT: Duration = Duration::from_secs(120);

/// The remote device a connection speaks for (`remote-origin.ts`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcRemoteOrigin {
    pub device_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_name: Option<String>,
}

/// `Refused`: the write never reached the prompt (not listed, outside a root, stale, too large);
/// `reason` names why.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserSkillDecision {
    Approved,
    Denied,
    Unconfirmed,
    Refused,
}

impl UserSkillDecision {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Approved => "approved",
            Self::Denied => "denied",
            Self::Unconfirmed => "unconfirmed",
            Self::Refused => "refused",
        }
    }
}

/// Callback matching `selectLocalGrant(select, title, choices)` on the local presentation.
pub type LocalGrantSelector = Arc<
    dyn Fn(String, Vec<String>, AbortSignal) -> BoxFuture<'static, Result<Option<String>, String>>
        + Send
        + Sync,
>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserSkillAuditEntry {
    pub origin: RpcRemoteOrigin,
    pub path: Option<String>,
    pub before: Option<String>,
    pub after: Option<String>,
    pub decision: UserSkillDecision,
    pub reason: Option<String>,
}

pub fn sha256_hex(bytes: Option<&[u8]>) -> Option<String> {
    bytes.map(|b| hex::encode(Sha256::digest(b)))
}

fn origin_name(origin: &RpcRemoteOrigin) -> &str {
    origin.device_name.as_deref().unwrap_or(&origin.device_id)
}

/// Who asks, which file, and exactly what changes: a new file's diff is all of its content.
pub fn user_skill_prompt_title(
    origin: &RpcRemoteOrigin,
    path: &str,
    before: &str,
    after: &str,
) -> String {
    let patch = create_unified_patch(path, path, before, after, 4);
    let char_count = patch.chars().count();
    let shown = if char_count > PROMPT_DIFF_MAX_CHARACTERS {
        let truncated: String = patch.chars().take(PROMPT_DIFF_MAX_CHARACTERS).collect();
        let remaining = char_count - PROMPT_DIFF_MAX_CHARACTERS;
        format!(
            "{truncated}\n... {remaining} more characters of the change are not shown; deny if unsure."
        )
    } else {
        patch
    };
    format!(
        "{} wants to change a user skill on this machine.\nFile: {path}\n\n{shown}",
        origin_name(origin)
    )
}

pub async fn ask_this_device(
    session: &AgentSession,
    title: String,
    selector: Option<&LocalGrantSelector>,
) -> UserSkillDecision {
    let Some(select) = selector else {
        let _has_presentation = maho_core::mcp_grant_presentations::get_mcp_grant_presentation(
            Some(&session.session_id()),
        );
        return UserSkillDecision::Unconfirmed;
    };
    let abort = AbortController::new();
    let fut = select(
        title,
        vec![USER_SKILL_ALLOW.to_owned(), USER_SKILL_DENY.to_owned()],
        abort.signal(),
    );
    let answer = match tokio::time::timeout(LOCAL_GRANT_TIMEOUT, fut).await {
        Ok(Ok(choice)) => choice,
        _ => None,
    };
    abort.abort(None);
    match answer.as_deref() {
        Some(USER_SKILL_ALLOW) => UserSkillDecision::Approved,
        Some(USER_SKILL_DENY) => UserSkillDecision::Denied,
        _ => UserSkillDecision::Unconfirmed,
    }
}

pub async fn append_user_skill_audit(
    agent_dir: &Path,
    entry: &UserSkillAuditEntry,
) -> std::io::Result<()> {
    let audit_dir = agent_dir.join("audit");
    tokio::fs::create_dir_all(&audit_dir).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ =
            tokio::fs::set_permissions(&audit_dir, std::fs::Permissions::from_mode(0o700)).await;
    }
    let mut line = json!({
        "at": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        "originDeviceId": entry.origin.device_id,
        "path": entry.path,
        "sha256Before": entry.before,
        "sha256After": entry.after,
        "decision": entry.decision.as_str(),
    });
    if let Some(name) = &entry.origin.device_name {
        line["originDeviceName"] = json!(name);
    }
    if let Some(reason) = &entry.reason {
        line["reason"] = json!(reason);
    }
    let serialized = format!("{line}\n");
    let audit_path = agent_dir.join(USER_SKILL_AUDIT_FILE);
    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        use std::io::Write;
        let mut opts = std::fs::OpenOptions::new();
        opts.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts.open(&audit_path)?;
        file.write_all(serialized.as_bytes())?;
        Ok(())
    })
    .await
    .map_err(|err| std::io::Error::other(err.to_string()))?
}

pub fn refused_by_device(
    decision: UserSkillDecision,
    origin: &RpcRemoteOrigin,
) -> ProjectWriteRefusal {
    if decision == UserSkillDecision::Denied {
        ProjectWriteRefusal {
            message: "The change was denied on the machine that holds the skill.".to_owned(),
            error_code: RPC_ERROR_USER_SKILL_WRITE_DECLINED.to_owned(),
            error_data: Some(json!({ "originDeviceId": origin.device_id })),
        }
    } else {
        ProjectWriteRefusal {
            message: "Nobody approved the change on the machine that holds the skill: it must be approved in an app open on that machine.".to_owned(),
            error_code: RPC_ERROR_USER_SKILL_WRITE_UNCONFIRMED.to_owned(),
            error_data: Some(json!({ "originDeviceId": origin.device_id })),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_title_includes_origin_and_truncates_long_diffs() {
        let origin = RpcRemoteOrigin {
            device_id: "dev-1".into(),
            device_name: Some("Laptop".into()),
        };
        let long = "a\n".repeat(12_000);
        let title =
            user_skill_prompt_title(&origin, "/home/u/.omo/agent/skills/s/SKILL.md", "", &long);
        assert!(title.starts_with("Laptop wants to change a user skill on this machine."));
        assert!(title.contains("more characters of the change are not shown; deny if unsure."));
    }

    #[tokio::test]
    async fn append_user_skill_audit_writes_jsonl_line() {
        let dir = tempfile::tempdir().expect("tempdir");
        append_user_skill_audit(
            dir.path(),
            &UserSkillAuditEntry {
                origin: RpcRemoteOrigin {
                    device_id: "dev-1".into(),
                    device_name: None,
                },
                path: Some("/skills/a/SKILL.md".into()),
                before: sha256_hex(Some(b"old")),
                after: sha256_hex(Some(b"new")),
                decision: UserSkillDecision::Approved,
                reason: None,
            },
        )
        .await
        .expect("audit write");
        let content =
            std::fs::read_to_string(dir.path().join(USER_SKILL_AUDIT_FILE)).expect("read audit");
        let parsed: serde_json::Value = serde_json::from_str(content.trim()).expect("valid json");
        assert_eq!(parsed["originDeviceId"], "dev-1");
        assert_eq!(parsed["decision"], "approved");
        assert!(parsed.get("originDeviceName").is_none());
    }
}
