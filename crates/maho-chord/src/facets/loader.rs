//! Port of `src/facets/loader.ts`.
use crate::types::{LoadedFacets, ServiceError};
use futures::FutureExt;

/// Start every disposal before awaiting completion, retaining input-order errors.
pub async fn dispose_loaded_facets(loaded: &mut [Box<dyn LoadedFacets>]) -> Vec<ServiceError> {
    futures::future::join_all(loaded.iter_mut().map(|entry| async move {
        let future = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let entry = entry;
            entry.dispose()
        }))
            .map_err(|_| crate::services::errors::contract("Loaded facet disposal panicked"))?;
        std::panic::AssertUnwindSafe(future).catch_unwind().await
            .map_err(|_| crate::services::errors::contract("Loaded facet disposal future panicked"))?
    }))
        .await
        .into_iter()
        .filter_map(Result::err)
        .collect()
}
