//! Port of senpi packages/coding-agent/src/core/execution/consent.ts.

use std::sync::Arc;

use maho_agent::ConsentClassifier;
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermissionAction {
    Allow,
    Ask,
    Deny,
}

pub fn consent_from_permission_policy<F>(policy: F) -> Arc<dyn ConsentClassifier>
where
    F: Fn(&str, &Value) -> PermissionAction + Send + Sync + 'static,
{
    struct PolicyConsent<F>(F);
    impl<F> ConsentClassifier for PolicyConsent<F>
    where
        F: Fn(&str, &Value) -> PermissionAction + Send + Sync,
    {
        fn needs_consent(&self, name: &str, args: &Value) -> bool {
            (self.0)(name, args) == PermissionAction::Ask
        }
    }
    Arc::new(PolicyConsent(policy))
}
