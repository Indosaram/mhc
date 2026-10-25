//! `fast-mode-inheritance.ts`: a delegated child preserves the parent's effective execution tier.
//!
//! When the parent runs on the priority ("fast") tier and the planned model has a `-fast` catalog
//! sibling, the child is planned on that sibling. The tier then travels as MODEL IDENTITY, which is
//! the one thing every child runner already carries (in-process `model`, rpc `--model`, persisted
//! `resolved_model`, resume), and the task row shows the `-fast` id instead of silently running
//! standard. A model already on `-fast`, or one with no priority sibling, is left as planned.

use senpi_task::{
    host::SenpiModelRegistry,
    manager::types::ResolvedChildPlan,
    state::{ResolvedModelRecord, ResolvedModelSource},
};
use serde_json::Value;

pub const FAST_MODEL_SUFFIX: &str = "-fast";

/// The parent's effective execution tier (`parentServiceTier()`).
pub type ResolveParentServiceTier = std::sync::Arc<dyn Fn() -> Option<String> + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelReference {
    pub provider: String,
    pub id: String,
}

fn split_model_reference(reference: &str) -> Option<ModelReference> {
    let slash = reference.find('/')?;
    if slash == 0 || slash + 1 == reference.len() {
        return None;
    }
    Some(ModelReference {
        provider: reference[..slash].to_string(),
        id: reference[slash + 1..].to_string(),
    })
}

/// `isPriorityAliasOf`: a catalog priority-tier alias of the base model. Same provider, a
/// `<base>-fast` id, `serviceTier: "priority"`, and an upstream id pointing back at the base. A model
/// that merely ENDS in `-fast` (a different Cursor composer SKU, a user entry with its own upstream
/// id) is not an alias and must not replace the planned model.
pub fn is_priority_alias_of(candidate: Option<&Value>, base: &ModelReference) -> bool {
    let Some(candidate) = candidate.and_then(Value::as_object) else {
        return false;
    };
    if candidate.get("provider").and_then(Value::as_str) != Some(base.provider.as_str()) {
        return false;
    }
    if candidate.get("id").and_then(Value::as_str) != Some(format!("{}{FAST_MODEL_SUFFIX}", base.id).as_str()) {
        return false;
    }
    if candidate.get("serviceTier").and_then(Value::as_str) != Some("priority") {
        return false;
    }
    let upstream = candidate
        .get("upstreamModelId")
        .or_else(|| candidate.get("upstream_model_id"))
        .and_then(Value::as_str);
    match upstream {
        None => true,
        Some(upstream) => upstream == base.id,
    }
}

fn as_fast_variant(candidate: Option<&Value>, base: &ModelReference) -> Option<String> {
    is_priority_alias_of(candidate, base).then(|| format!("{}/{}{FAST_MODEL_SUFFIX}", base.provider, base.id))
}

/// `inheritParentFastMode`: only a priority parent with a true sibling changes the plan.
pub fn inherit_parent_fast_mode(
    plan: ResolvedChildPlan,
    registry: Option<&dyn SenpiModelRegistry>,
    parent_service_tier: Option<&str>,
) -> ResolvedChildPlan {
    let (Some(registry), Some("priority")) = (registry, parent_service_tier) else {
        return plan;
    };
    let Some(base) = split_model_reference(&plan.model) else {
        return plan;
    };
    if base.id.ends_with(FAST_MODEL_SUFFIX) {
        return plan;
    }
    let candidate = registry.find(&base.provider, &format!("{}{FAST_MODEL_SUFFIX}", base.id));
    let Some(model) = as_fast_variant(candidate.as_ref(), &base) else {
        return plan;
    };
    let resolved_model = plan.resolved_model.clone().map(|record| ResolvedModelRecord {
        model_id: format!("{}{FAST_MODEL_SUFFIX}", base.id),
        display: model.clone(),
        ..record
    });
    ResolvedChildPlan {
        model,
        resolved_model: resolved_model.or(Some(ResolvedModelRecord::new(
            ResolvedModelSource::Explicit,
            &base.provider,
            &format!("{}{FAST_MODEL_SUFFIX}", base.id),
        ))),
        ..plan
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn base() -> ModelReference {
        ModelReference {
            provider: "openai".into(),
            id: "gpt-5".into(),
        }
    }

    #[test]
    fn a_priority_alias_needs_the_tier_and_the_upstream_pointer() {
        assert!(is_priority_alias_of(
            Some(&json!({"provider":"openai","id":"gpt-5-fast","serviceTier":"priority","upstreamModelId":"gpt-5"})),
            &base()
        ));
        // A different SKU that merely ends in -fast is NOT an alias.
        assert!(!is_priority_alias_of(
            Some(&json!({"provider":"cursor","id":"gpt-5-fast","serviceTier":"priority","upstreamModelId":"composer"})),
            &base()
        ));
        assert!(!is_priority_alias_of(
            Some(&json!({"provider":"openai","id":"gpt-5-fast","upstreamModelId":"gpt-5"})),
            &base()
        ));
        assert!(!is_priority_alias_of(None, &base()));
    }

    #[test]
    fn a_standard_parent_keeps_the_planned_model() {
        let plan = ResolvedChildPlan {
            model: "openai/gpt-5".into(),
            ..ResolvedChildPlan::default()
        };
        assert_eq!(
            inherit_parent_fast_mode(plan.clone(), None, Some("priority")).model,
            "openai/gpt-5"
        );
        assert_eq!(
            inherit_parent_fast_mode(plan, None, Some("standard")).model,
            "openai/gpt-5"
        );
    }

    #[test]
    fn a_model_already_on_fast_is_left_alone() {
        let plan = ResolvedChildPlan {
            model: "openai/gpt-5-fast".into(),
            ..ResolvedChildPlan::default()
        };
        assert_eq!(
            inherit_parent_fast_mode(plan, None, Some("priority")).model,
            "openai/gpt-5-fast"
        );
    }
}
