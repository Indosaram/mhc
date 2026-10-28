//! `manager/parent-registry-context.ts`: thread the parent session's live model registry into
//! every in-process child, and resolve resume-time models fail-closed against it.

use std::any::Any;
use std::sync::Arc;

use crate::host::parse_model;
use crate::manager::runner::{
    InProcessSessionContext, InProcessSessionContextProvider, ResumeSessionContextResult,
};
use crate::manager::types::ManagedStartSpec;
use crate::model_exclusion::is_excluded_model;
use crate::runners::in_process::child_options::HostHandle;
use crate::senpi::as_senpi_thinking_level;

/// The parent session's concrete host model registry (`ChildModelRegistry`). The child must receive
/// this exact object so it resolves the same provider set, including providers registered
/// dynamically on the parent.
pub trait ChildModelRegistry: Any + Send + Sync {
    /// The host `Model` for `provider`/`model_id`, when the registry knows it.
    fn find(&self, provider: &str, model_id: &str) -> Option<HostHandle>;
    /// The auth storage bound to this registry.
    fn auth_storage(&self) -> HostHandle;
    /// The registry's model runtime, when it exposes one.
    fn model_runtime(&self) -> Option<HostHandle>;
}

/// Returns the parent's live registry; `None` before the first live context, so the child keeps
/// the host's default resolution instead of spawning against a half-built registry.
pub type ParentModelRegistryResolver =
    Arc<dyn Fn() -> Option<Arc<dyn ChildModelRegistry>> + Send + Sync>;

/// Resolve a canonical `provider/modelId` reference. Only the FIRST slash splits, so an
/// openrouter-style id keeps its inner slashes; an absent or edge slash yields `None` without a
/// lookup.
pub fn find_model_reference<M>(
    find: impl FnOnce(&str, &str) -> Option<M>,
    model_reference: &str,
) -> Option<M> {
    let parsed = parse_model(model_reference)?;
    find(&parsed.provider, &parsed.model_id)
}

/// The provider returned by [`create_parent_registry_session_context`].
pub struct ParentRegistrySessionContext {
    resolve_registry: ParentModelRegistryResolver,
    exclude_models: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
}

/// `createParentRegistrySessionContext`.
pub fn create_parent_registry_session_context(
    resolve_registry: ParentModelRegistryResolver,
) -> ParentRegistrySessionContext {
    ParentRegistrySessionContext {
        resolve_registry,
        exclude_models: Arc::new(Vec::new),
    }
}

impl ParentRegistrySessionContext {
    /// Read exclusions at each launch/resume, not when the parent context is constructed.
    pub fn with_exclude_models(
        mut self,
        resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    ) -> Self {
        self.exclude_models = resolver;
        self
    }
}

fn registry_context(
    registry: Arc<dyn ChildModelRegistry>,
    model: Option<HostHandle>,
    spec: &ManagedStartSpec,
) -> InProcessSessionContext {
    InProcessSessionContext {
        auth_storage: Some(registry.auth_storage()),
        model_runtime: registry.model_runtime(),
        model,
        thinking_level: as_senpi_thinking_level(spec.variant.as_deref())
            .map(|level| level.as_str().to_string()),
        model_registry: Some(registry as HostHandle),
        agent_dir: None,
    }
}

impl InProcessSessionContextProvider for ParentRegistrySessionContext {
    fn excluded_models(&self) -> Vec<String> {
        (self.exclude_models)()
    }
    fn validate_start(&self, spec: &ManagedStartSpec) -> Result<(), String> {
        let selected = spec
            .resolved_model
            .as_ref()
            .map(|model| format!("{}/{}", model.provider, model.model_id))
            .or_else(|| spec.model.clone());
        if let Some(model) = selected.as_deref()
            && is_excluded_model(model, &(self.exclude_models)()) {
                return Err(format!("model_unavailable: excluded model {model}"));
            }
        Ok(())
    }
    fn provide(&self, spec: &ManagedStartSpec) -> InProcessSessionContext {
        let Some(registry) = (self.resolve_registry)() else {
            return InProcessSessionContext::default();
        };
        let model = spec.model.as_deref().and_then(|reference| {
            find_model_reference(|provider, id| registry.find(provider, id), reference)
        });
        registry_context(registry, model, spec)
    }

    fn resolve_resume_context(
        &self,
        spec: &ManagedStartSpec,
    ) -> Option<ResumeSessionContextResult> {
        let patterns = (self.exclude_models)();
        let reference = spec
            .resolved_model
            .as_ref()
            .map(|m| format!("{}/{}", m.provider, m.model_id))
            .or_else(|| spec.model.clone());
        if reference
            .as_deref()
            .is_some_and(|model| is_excluded_model(model, &patterns))
        {
            return Some(Err(format!(
                "model_unavailable: excluded model {}",
                reference.unwrap_or_default()
            )));
        }
        Some(resolve_resume_context(&self.resolve_registry, spec))
    }
}

/// Resume-time resolution that FAILS CLOSED: the persisted `provider` + `model_id` (never the
/// human `display`) must resolve in the live registry, else a `model_unavailable` reason. Resume
/// never drifts to the host's default model.
pub fn resolve_resume_context(
    resolve_registry: &ParentModelRegistryResolver,
    spec: &ManagedStartSpec,
) -> ResumeSessionContextResult {
    let registry =
        resolve_registry().ok_or_else(|| "no live parent model registry available".to_string())?;
    let resolved = spec
        .resolved_model
        .as_ref()
        .ok_or_else(|| "spec carries no resolved_model to match against".to_string())?;
    let model = registry
        .find(&resolved.provider, &resolved.model_id)
        .ok_or_else(|| {
            format!(
                "provider \"{}\" model \"{}\" not found in the live registry",
                resolved.provider, resolved.model_id
            )
        })?;
    Ok(registry_context(registry, Some(model), spec))
}

/// Compatibility constructor with fixed exclusions.
pub fn create_parent_registry_session_context_with_exclusions(
    resolve_registry: ParentModelRegistryResolver,
    exclude_models: Vec<String>,
) -> ParentRegistrySessionContext {
    create_parent_registry_session_context(resolve_registry)
        .with_exclude_models(Arc::new(move || exclude_models.clone()))
}

#[cfg(test)]
mod exclusion_tests {
    use super::*;
    #[test]
    fn excluded_selected_model_never_reaches_registry() {
        let context = create_parent_registry_session_context(Arc::new(|| {
            panic!("excluded model reached registry")
        }))
        .with_exclude_models(Arc::new(|| vec!["blocked/*".into()]));
        let spec = ManagedStartSpec {
            model: Some("blocked/model".into()),
            resolved_model: Some(crate::state::ResolvedModelRecord::new(
                crate::state::ResolvedModelSource::Category,
                "blocked",
                "model",
            )),
            ..Default::default()
        };
        assert!(context.validate_start(&spec).is_err());
        assert!(
            context
                .resolve_resume_context(&spec)
                .expect("resolver")
                .is_err()
        );
    }
}
