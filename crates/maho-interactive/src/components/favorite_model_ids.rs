//! The subset of components/model-favorites.ts the selector components need.
//!
//! senpi keeps `getModelFullId`/`isFavoriteModel`/`toggleFavoriteModel`/`favoriteModels`/
//! `clearFavoriteModels`/`moveFavoriteModel`/`getSortedFavoriteModelIds` in
//! `components/model-favorites.ts`. The full module (including
//! `mergeFavoritePatternsForPersist`, which needs the todo-17 `PatternResolution` type) is the
//! todo-35 module; the pure id helpers the todo-33 selectors call live here so the components stay
//! faithful without importing an unported module. `null` in senpi means "all models are favorites",
//! modelled here as `None`.

use crate::model_search::ModelSearchItem;

pub type FavoriteModelIds = Option<Vec<String>>;

/// senpi's `getModelFullId(model)`.
pub fn get_model_full_id(item: &ModelSearchItem) -> String {
    format!("{}/{}", item.provider, item.id)
}

/// senpi's `isFavoriteModel`.
pub fn is_favorite_model(favorite_ids: &FavoriteModelIds, id: &str) -> bool {
    match favorite_ids {
        None => true,
        Some(ids) => ids.iter().any(|candidate| candidate == id),
    }
}

/// senpi's `toggleFavoriteModel`.
pub fn toggle_favorite_model(
    favorite_ids: &FavoriteModelIds,
    all_ids: &[String],
    id: &str,
) -> FavoriteModelIds {
    match favorite_ids {
        None => Some(all_ids.iter().filter(|candidate| *candidate != id).cloned().collect()),
        Some(ids) => {
            if let Some(index) = ids.iter().position(|candidate| candidate == id) {
                let mut next = ids.clone();
                next.remove(index);
                Some(next)
            } else {
                let mut next = ids.clone();
                next.push(id.to_owned());
                Some(next)
            }
        }
    }
}

/// senpi's `favoriteModels`.
pub fn favorite_models(
    favorite_ids: &FavoriteModelIds,
    all_ids: &[String],
    target_ids: Option<&[String]>,
) -> FavoriteModelIds {
    let Some(ids) = favorite_ids else {
        return None;
    };
    let targets = target_ids.unwrap_or(all_ids);
    let mut result = ids.clone();
    for id in targets {
        if !result.contains(id) {
            result.push(id.clone());
        }
    }
    if result.len() == all_ids.len() {
        None
    } else {
        Some(result)
    }
}

/// senpi's `clearFavoriteModels`.
pub fn clear_favorite_models(
    favorite_ids: &FavoriteModelIds,
    all_ids: &[String],
    target_ids: Option<&[String]>,
) -> FavoriteModelIds {
    let Some(ids) = favorite_ids else {
        return Some(match target_ids {
            Some(targets) => all_ids.iter().filter(|id| !targets.contains(id)).cloned().collect(),
            None => Vec::new(),
        });
    };
    let targets: Vec<String> = target_ids.map(<[String]>::to_vec).unwrap_or_else(|| ids.clone());
    Some(ids.iter().filter(|id| !targets.contains(id)).cloned().collect())
}

/// senpi's `moveFavoriteModel`.
pub fn move_favorite_model(favorite_ids: &FavoriteModelIds, id: &str, delta: i64) -> FavoriteModelIds {
    let Some(ids) = favorite_ids else {
        return None;
    };
    let Some(index) = ids.iter().position(|candidate| candidate == id) else {
        return Some(ids.clone());
    };
    let new_index = index as i64 + delta;
    if new_index < 0 || new_index as usize >= ids.len() {
        return Some(ids.clone());
    }
    let mut result = ids.clone();
    result.swap(index, new_index as usize);
    Some(result)
}

/// senpi's `getSortedFavoriteModelIds`.
pub fn get_sorted_favorite_model_ids(favorite_ids: &FavoriteModelIds, all_ids: &[String]) -> Vec<String> {
    match favorite_ids {
        None => all_ids.to_vec(),
        Some(ids) => {
            let mut result = ids.clone();
            result.extend(all_ids.iter().filter(|id| !ids.contains(id)).cloned());
            result
        }
    }
}
