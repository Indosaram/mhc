//! Round-robin model distribution (`omo-config-core/src/models/models-distribution.ts`).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};

use omo_config_core::split_reasoning_suffix;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelsDistribution {
    Sequential,
    RoundRobin,
}

impl ModelsDistribution {
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "sequential" => Some(Self::Sequential),
            "round_robin" => Some(Self::RoundRobin),
            _ => None,
        }
    }
}

/// Own one distributor per spawn planner, not per resolution or model retry (`createModelDistributor`).
#[derive(Clone, Default)]
pub struct ModelDistributor {
    cursors: Arc<Mutex<HashMap<String, usize>>>,
}

impl std::fmt::Debug for ModelDistributor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelDistributor").finish_non_exhaustive()
    }
}

impl PartialEq for ModelDistributor {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.cursors, &other.cursors)
    }
}

impl Eq for ModelDistributor {}

impl ModelDistributor {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn distribute<T: Clone>(
        &self,
        target: &str,
        models: &[T],
        distribution: Option<ModelsDistribution>,
        model_of: impl Fn(&T) -> &str,
    ) -> Vec<T> {
        if distribution != Some(ModelsDistribution::RoundRobin) || models.is_empty() {
            return models.to_vec();
        }
        let mut seen = HashSet::new();
        let mut unique = Vec::with_capacity(models.len());
        for entry in models {
            let model = model_of(entry);
            let key = canonical_model_key(model);
            if seen.insert(key) {
                unique.push(entry.clone());
            }
        }
        if unique.is_empty() {
            return Vec::new();
        }
        let mut cursors = self.cursors.lock().unwrap_or_else(PoisonError::into_inner);
        let start = cursors.get(target).copied().unwrap_or(0) % unique.len();
        cursors.insert(target.to_owned(), (start + 1) % unique.len());
        let mut rotated = Vec::with_capacity(unique.len());
        rotated.extend_from_slice(&unique[start..]);
        rotated.extend_from_slice(&unique[..start]);
        rotated
    }
}

fn canonical_model_key(model: &str) -> String {
    let split = split_reasoning_suffix(model, Some(true));
    let mut base = split.base.trim();
    if let Some(open) = base.rfind('(')
        && base.ends_with(')')
    {
        base = base[..open].trim();
    }
    if let Some((head, tail)) = base.rsplit_once(|c: char| c.is_whitespace())
        && !head.is_empty()
        && tail
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_alphabetic())
        && tail
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
    {
        base = head.trim();
    }
    base.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequential_keeps_order_and_round_robin_rotates_unique_models_per_target() {
        let distributor = ModelDistributor::new();
        let entries = vec![
            "openai/gpt-6-astra".to_string(),
            "openai/gpt-6-astra high".to_string(),
            "anthropic/claude-opus-5-5".to_string(),
        ];
        assert_eq!(
            distributor.distribute(
                "category:one",
                &entries,
                Some(ModelsDistribution::Sequential),
                String::as_str,
            ),
            entries
        );
        assert_eq!(
            distributor.distribute(
                "category:one",
                &entries,
                Some(ModelsDistribution::RoundRobin),
                String::as_str,
            ),
            vec![
                "openai/gpt-6-astra".to_string(),
                "anthropic/claude-opus-5-5".to_string()
            ]
        );
        assert_eq!(
            distributor.distribute(
                "category:one",
                &entries,
                Some(ModelsDistribution::RoundRobin),
                String::as_str,
            ),
            vec![
                "anthropic/claude-opus-5-5".to_string(),
                "openai/gpt-6-astra".to_string()
            ]
        );
    }
}
