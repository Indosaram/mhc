//! Port of `packages/coding-agent/src/core/tools/tool-machine.ts`.

use crate::definition::{AbortSignal, ToolError, ToolFuture};
use maho_machine_ops::{
    BeforeOpHook, Limits, MachineOpError, MachineOps, OpContext, OpErrorCode, OpName, OpPrincipal,
    OpScope, PathLockMode, PolicyDecision, PolicyHooks, PrincipalKind,
};
use std::sync::Arc;

/// The hooks the tool path honours: only `beforeOp`, once per tool call.
#[derive(Clone, Default)]
pub struct ToolPathPolicy {
    pub before_op: Option<BeforeOpHook>,
}

/// One `MachineOps` bound to the tool path: principal `local-session`, no scope root (the tools resolve paths).
#[derive(Clone)]
pub struct ToolMachine {
    resolve_ops: Arc<dyn Fn() -> Arc<dyn MachineOps> + Send + Sync>,
    policy: Option<ToolPathPolicy>,
}

fn tool_path_limits(machine_limits: Limits) -> Limits {
    Limits {
        read_max_bytes: None,
        exec_stdout_max_bytes: 0,
        exec_stderr_max_bytes: 0,
        exec_timeout_default_ms: None,
        ..machine_limits
    }
}

impl ToolMachine {
    pub fn from_resolver(
        resolve_ops: Arc<dyn Fn() -> Arc<dyn MachineOps> + Send + Sync>,
        policy: Option<ToolPathPolicy>,
    ) -> Self {
        Self {
            resolve_ops,
            policy,
        }
    }

    pub fn new(ops: Arc<dyn MachineOps>, policy: Option<ToolPathPolicy>) -> Self {
        Self::from_resolver(Arc::new(move || Arc::clone(&ops)), policy)
    }

    pub fn ops(&self) -> Arc<dyn MachineOps> {
        (self.resolve_ops)()
    }

    pub fn has_before_op(&self) -> bool {
        self.policy.as_ref().is_some_and(|p| p.before_op.is_some())
    }

    /// Runs `PolicyHooks.beforeOp` once for one tool call; returns `None` synchronously when no hook is installed.
    pub fn begin<'a>(
        &'a self,
        op: OpName,
        args: serde_json::Value,
        signal: Option<&AbortSignal>,
    ) -> Option<ToolFuture<'a, ()>> {
        let before_op = self.policy.as_ref()?.before_op.clone()?;
        let mut ctx = self.context(signal, None);
        ctx.policy = PolicyHooks {
            filesystem: None,
            before_op: Some(before_op.clone()),
            after_op: None,
        };
        Some(Box::pin(async move {
            let decision = before_op(op, &args, &ctx)
                .await
                .map_err(|err| match err.code {
                    OpErrorCode::Aborted => ToolError::Aborted,
                    _ => ToolError::Message(err.message),
                })?;
            match decision {
                PolicyDecision::Allow => Ok(()),
                PolicyDecision::Deny { reason } => Err(ToolError::Message(reason)),
            }
        }))
    }

    /// Context for a sub-operation inside a call `begin` already admitted.
    pub fn context(
        &self,
        signal: Option<&AbortSignal>,
        path_lock: Option<PathLockMode>,
    ) -> OpContext {
        OpContext {
            scope: OpScope {
                id: "local-session".to_owned(),
                root: None,
            },
            principal: OpPrincipal {
                kind: PrincipalKind::LocalSession,
                id: "local-session".to_owned(),
            },
            signal: signal
                .map(AbortSignal::to_machine_signal)
                .unwrap_or_default(),
            limits: tool_path_limits(self.ops().limits()),
            policy: PolicyHooks::default(),
            audit: None,
            path_lock,
        }
    }
}

pub fn bind_tool_machine(
    resolve_ops: Arc<dyn Fn() -> Arc<dyn MachineOps> + Send + Sync>,
    policy: Option<ToolPathPolicy>,
) -> ToolMachine {
    ToolMachine::from_resolver(resolve_ops, policy)
}

pub fn create_tool_machine(
    ops: Arc<dyn MachineOps>,
    policy: Option<ToolPathPolicy>,
) -> ToolMachine {
    ToolMachine::new(ops, policy)
}

pub fn tool_error_from_op(err: MachineOpError) -> ToolError {
    match err.code {
        OpErrorCode::Aborted => ToolError::Aborted,
        _ => ToolError::Message(err.message),
    }
}
