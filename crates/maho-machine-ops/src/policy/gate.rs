//! Port of `packages/machine-ops/src/policy/gate.ts`.

use crate::errors::{MachineOpError, OpErrorCode};
use crate::policy::filesystem::{
    FilesystemOperation, FilesystemPolicyDecision, FilesystemPolicyRequest,
    canonicalize_filesystem_path,
};
use crate::types::{AuditEntry, AuditVerdict, OpContext, OpName, OpOutcome, PolicyDecision};
use std::future::Future;
use std::path::{Component, Path, PathBuf};

/// Resolve a request path against the scope root (or the process cwd when the scope has none).
pub fn resolve_op_path(path: &str, ctx: &OpContext) -> Result<PathBuf, MachineOpError> {
    let base = match &ctx.scope.root {
        Some(root) => root.clone(),
        None => std::env::current_dir()?,
    };
    Ok(normalize_resolve(&base, Path::new(path)))
}

/// Lexical path resolution matching `path.resolve(base, path)` (applies `..` and `.`, handles absolute `path`).
pub(crate) fn normalize_resolve(base: &Path, path: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    let mut out = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                // Don't pop past RootDir or Prefix.
                let can_pop = matches!(out.components().next_back(), Some(Component::Normal(_)));
                if can_pop {
                    out.pop();
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn contains(root: &Path, target: &Path) -> bool {
    target.starts_with(root)
}

/// Containment and filesystem policy for one path, before any effect.
pub async fn check_path(
    absolute_path: &Path,
    operation: FilesystemOperation,
    op: OpName,
    ctx: &OpContext,
) -> Result<(), MachineOpError> {
    let root = ctx.scope.root.as_deref();
    let filesystem = ctx.policy.filesystem.as_ref();
    if root.is_none() && filesystem.is_none() {
        return Ok(());
    }
    let canonical_path = canonicalize_filesystem_path(absolute_path).await?;
    if let Some(root) = root {
        let canonical_root = canonicalize_filesystem_path(root).await?;
        if !contains(&canonical_root, &canonical_path) {
            if let Some(audit) = &ctx.audit {
                audit(AuditEntry {
                    op,
                    principal: ctx.principal.clone(),
                    target: canonical_path.to_string_lossy().into_owned(),
                    verdict: AuditVerdict::Denied,
                });
            }
            return Err(MachineOpError::new(
                OpErrorCode::PolicyDenied,
                format!("{} is outside the scope root", absolute_path.display()),
            ));
        }
    }
    if let Some(filesystem) = filesystem {
        let decision = filesystem(FilesystemPolicyRequest {
            operation,
            canonical_path: canonical_path.clone(),
            tool_name: op.as_str().to_owned(),
        })
        .await?;
        if let FilesystemPolicyDecision::Deny { reason } = decision {
            if let Some(audit) = &ctx.audit {
                audit(AuditEntry {
                    op,
                    principal: ctx.principal.clone(),
                    target: canonical_path.to_string_lossy().into_owned(),
                    verdict: AuditVerdict::Denied,
                });
            }
            return Err(MachineOpError::new(OpErrorCode::PolicyDenied, reason));
        }
    }
    Ok(())
}

/// Run `beforeOp`, the effect, then `afterOp`. A denial returns `PolicyDenied` before the effect runs.
pub async fn gated<T, F, Fut>(
    op: OpName,
    args: &serde_json::Value,
    ctx: &OpContext,
    effect: F,
) -> Result<T, MachineOpError>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<T, MachineOpError>>,
{
    if ctx.signal.is_aborted() {
        return Err(MachineOpError::new(
            OpErrorCode::Aborted,
            "Operation aborted",
        ));
    }
    if let Some(before_op) = &ctx.policy.before_op {
        let decision = before_op(op, args, ctx).await?;
        if let PolicyDecision::Deny { reason } = decision {
            if let Some(audit) = &ctx.audit {
                audit(AuditEntry {
                    op,
                    principal: ctx.principal.clone(),
                    target: describe_target(args),
                    verdict: AuditVerdict::Denied,
                });
            }
            return Err(MachineOpError::new(OpErrorCode::PolicyDenied, reason));
        }
    }
    if let Some(audit) = &ctx.audit {
        audit(AuditEntry {
            op,
            principal: ctx.principal.clone(),
            target: describe_target(args),
            verdict: AuditVerdict::Allowed,
        });
    }
    match effect().await {
        Ok(value) => {
            notify_after(op, args, OpOutcome::Ok, ctx).await?;
            Ok(value)
        }
        Err(error) => {
            notify_after(op, args, OpOutcome::Err(error.clone()), ctx).await?;
            Err(error)
        }
    }
}

async fn notify_after(
    op: OpName,
    args: &serde_json::Value,
    outcome: OpOutcome,
    ctx: &OpContext,
) -> Result<(), MachineOpError> {
    if let Some(after_op) = &ctx.policy.after_op {
        after_op(op, args, outcome, ctx).await?;
    }
    Ok(())
}

/// The audit target: a path, or the first 64 characters of a command; never a payload.
fn describe_target(args: &serde_json::Value) -> String {
    let Some(obj) = args.as_object() else {
        return String::new();
    };
    if let Some(path) = obj.get("path").and_then(|v| v.as_str()) {
        return path.to_owned();
    }
    if let Some(command) = obj.get("command").and_then(|v| v.as_str()) {
        return command.chars().take(64).collect();
    }
    if let Some(topic) = obj.get("topic").and_then(|v| v.as_str()) {
        return topic.to_owned();
    }
    String::new()
}
