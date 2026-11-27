//! `category-config-generation.ts`: tracks generations of the resolved builtin category map.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, PoisonError},
};

use senpi_task::{
    category::{
        BUILTIN_CATEGORY_DEFAULTS, CategoryResolutionResult, ResolveCategoryOptions,
        resolve_category,
    },
    host::SenpiModelRegistry,
    manager::types::{ChildPlanner, ManagerStartSpec, PlanResolutionError, ResolvedChildPlan},
};
use serde_json::Value;

use crate::planner::ResolveModelRegistry;

pub type ConfigScopedChildPlanner = Arc<
    dyn Fn(&ManagerStartSpec, &Value) -> Result<ResolvedChildPlan, Box<PlanResolutionError>>
        + Send
        + Sync,
>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaskedCategoryModel {
    pub provider: String,
    pub model_id: String,
}

/// Masking seam for the canonical map. Defaults to identity; the telemetry component can inject a
/// provider/model allowlist mask so a generation only changes when the exportable map changes.
pub type MaskCategoryModel = Arc<dyn Fn(&str, &str) -> MaskedCategoryModel + Send + Sync>;

/// The effective category map as it stood when a task was planned. `categories` holds one closed
/// value per builtin category (`<provider>/<model_id>/<reasoning>`, or `disabled`/`unavailable`);
/// user-defined category names never enter the snapshot, only their counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategoryConfigSnapshot {
    pub generation: u64,
    pub categories: BTreeMap<String, String>,
    pub user_category_count: usize,
    pub builtin_overridden_count: usize,
}

struct GenerationState {
    snapshot: Option<CategoryConfigSnapshot>,
    canonical: Option<String>,
}

pub struct CategoryConfigGenerations {
    mask: MaskCategoryModel,
    state: Mutex<GenerationState>,
}

fn identity_mask(provider: &str, model_id: &str) -> MaskedCategoryModel {
    MaskedCategoryModel {
        provider: provider.to_owned(),
        model_id: model_id.to_owned(),
    }
}

fn encode_category(
    name: &str,
    omo_config: &Value,
    registry: &dyn SenpiModelRegistry,
    mask: &MaskCategoryModel,
) -> String {
    match resolve_category(
        name,
        omo_config,
        registry,
        &ResolveCategoryOptions::default(),
    ) {
        Ok(CategoryResolutionResult::Resolved { spec, .. }) => {
            let masked = mask(&spec.provider, &spec.model_id);
            let reasoning = spec
                .reasoning
                .or(spec.reasoning_effort)
                .or(spec.variant)
                .unwrap_or_else(|| "none".to_string());
            format!("{}/{}/{}", masked.provider, masked.model_id, reasoning)
        }
        Ok(CategoryResolutionResult::Disabled { .. }) => "disabled".to_string(),
        Ok(CategoryResolutionResult::ModelUnavailable(_)) | Err(_) => "unavailable".to_string(),
        Ok(CategoryResolutionResult::NotFound { .. }) => "absent".to_string(),
    }
}

fn canonical_categories(
    omo_config: &Value,
    registry: &dyn SenpiModelRegistry,
    mask: &MaskCategoryModel,
) -> BTreeMap<String, String> {
    BUILTIN_CATEGORY_DEFAULTS
        .iter()
        .map(|definition| {
            (
                definition.name.to_owned(),
                encode_category(definition.name, omo_config, registry, mask),
            )
        })
        .collect()
}

fn canonical_form(
    categories: &BTreeMap<String, String>,
    user_category_count: usize,
    builtin_overridden_count: usize,
) -> String {
    let entries: Vec<String> = categories
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect();
    format!(
        "{}|u={user_category_count}|o={builtin_overridden_count}",
        entries.join(";")
    )
}

impl CategoryConfigGenerations {
    pub fn new(mask: Option<MaskCategoryModel>) -> Self {
        Self {
            mask: mask.unwrap_or_else(|| Arc::new(identity_mask)),
            state: Mutex::new(GenerationState {
                snapshot: None,
                canonical: None,
            }),
        }
    }

    pub fn observe(
        &self,
        omo_config: &Value,
        registry: &dyn SenpiModelRegistry,
    ) -> CategoryConfigSnapshot {
        let categories = canonical_categories(omo_config, registry, &self.mask);
        let user_names: Vec<&str> = omo_config
            .get("categories")
            .and_then(Value::as_object)
            .map(|object| object.keys().map(String::as_str).collect())
            .unwrap_or_default();
        let is_builtin = |name: &str| {
            BUILTIN_CATEGORY_DEFAULTS
                .iter()
                .any(|definition| definition.name == name)
        };
        let user_category_count = user_names.iter().filter(|name| !is_builtin(name)).count();
        let builtin_overridden_count = user_names.iter().filter(|name| is_builtin(name)).count();
        let form = canonical_form(&categories, user_category_count, builtin_overridden_count);

        let mut guard = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(previous) = &guard.snapshot
            && guard.canonical.as_deref() == Some(form.as_str())
        {
            return previous.clone();
        }
        let generation = guard
            .snapshot
            .as_ref()
            .map_or(0, |previous| previous.generation + 1);
        let created = CategoryConfigSnapshot {
            generation,
            categories,
            user_category_count,
            builtin_overridden_count,
        };
        guard.snapshot = Some(created.clone());
        guard.canonical = Some(form);
        created
    }

    pub fn current(&self) -> Option<CategoryConfigSnapshot> {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .snapshot
            .clone()
    }
}

pub fn create_category_config_generations(
    mask: Option<MaskCategoryModel>,
) -> Arc<CategoryConfigGenerations> {
    Arc::new(CategoryConfigGenerations::new(mask))
}

/// Observe the effective category map at the planner seam.
pub fn create_generation_observing_planner(
    planner: ConfigScopedChildPlanner,
    omo_config: Arc<dyn Fn() -> Value + Send + Sync>,
    resolve_registry: ResolveModelRegistry,
    generations: Arc<CategoryConfigGenerations>,
) -> ChildPlanner {
    Arc::new(move |spec| {
        let plan_config = omo_config();
        let resolution = planner(spec, &plan_config);
        if resolution.is_ok()
            && let Some(registry) = resolve_registry()
        {
            generations.observe(&plan_config, registry.as_ref());
        }
        resolution
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use senpi_task::host::HostError;
    use serde_json::json;

    struct StaticRegistry(Vec<Value>);

    impl SenpiModelRegistry for StaticRegistry {
        fn get_available(&self) -> Result<Value, HostError> {
            Ok(Value::Array(self.0.clone()))
        }
        fn find(&self, provider: &str, model_id: &str) -> Option<Value> {
            self.0
                .iter()
                .find(|entry| {
                    entry.get("provider").and_then(Value::as_str) == Some(provider)
                        && entry.get("id").and_then(Value::as_str) == Some(model_id)
                })
                .cloned()
        }
    }

    #[test]
    fn bumps_generation_only_when_canonical_category_map_changes() {
        let registry = StaticRegistry(vec![
            json!({"provider": "google", "id": "gemini-4-argon"}),
            json!({"provider": "anthropic", "id": "claude-fable-5-2"}),
        ]);
        let generations = create_category_config_generations(None);
        assert!(generations.current().is_none());

        let base = json!({"categories": {"quick": {"model": "google/gemini-4-argon"}}});
        let snap0 = generations.observe(&base, &registry);
        assert_eq!(snap0.generation, 0);
        assert_eq!(snap0.builtin_overridden_count, 1);
        assert_eq!(snap0.user_category_count, 0);

        let snap_same = generations.observe(&base, &registry);
        assert_eq!(snap_same.generation, 0);

        let updated = json!({"categories": {"quick": {"model": "anthropic/claude-fable-5-2"}}});
        let snap1 = generations.observe(&updated, &registry);
        assert_eq!(snap1.generation, 1);
        assert_eq!(generations.current().expect("current").generation, 1);
    }
}
