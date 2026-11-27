//! Port of senpi packages/coding-agent/src/core/execution/lifecycle.ts.

use std::sync::Arc;

use maho_agent::StoreValue;
use maho_agent::execution::scope_lifecycle::{
    LifecycleCommitResult, LifecycleHandler, ScopeError, ScopeLifecycle, ScopeRefusal,
};
use maho_ai::types::BoxFuture;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("lifecycle event {kind} refused: {reason:?}")]
pub struct LifecycleRefusedError {
    pub kind: String,
    pub reason: ScopeRefusal,
}

pub struct LifecycleBridge {
    scopes: Arc<dyn ScopeLifecycle>,
    issuer: String,
    scope: String,
}

impl LifecycleBridge {
    pub fn new(
        scopes: Arc<dyn ScopeLifecycle>,
        issuer: impl Into<String>,
        scope: impl Into<String>,
    ) -> Self {
        Self {
            scopes,
            issuer: issuer.into(),
            scope: scope.into(),
        }
    }

    pub async fn event(&self, kind: &str, handler: LifecycleHandler) -> Result<usize, String> {
        match self
            .scopes
            .lifecycle(&self.issuer, &self.scope, kind, handler)
            .await?
        {
            LifecycleCommitResult::Committed { seq } => Ok(seq),
            LifecycleCommitResult::Refused(reason) => Err(LifecycleRefusedError {
                kind: kind.to_owned(),
                reason,
            }
            .to_string()),
        }
    }

    pub async fn call(
        &self,
        run: Box<dyn FnOnce() -> BoxFuture<'static, Result<StoreValue, ScopeError>> + Send>,
    ) -> Result<StoreValue, ScopeError> {
        self.scopes.call_scope(&self.scope, run).await
    }
}
