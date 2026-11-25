//! Port of `packages/coding-agent/src/engine-trust/adopted.ts` (omo#10762).

use std::sync::{PoisonError, RwLock};

use super::marker::EngineTrustMarker;

static ADOPTED: RwLock<Option<EngineTrustMarker>> = RwLock::new(None);

pub fn record_adopted_engine_trust(marker: EngineTrustMarker) {
    *ADOPTED.write().unwrap_or_else(PoisonError::into_inner) = Some(marker);
}

pub fn adopted_engine_trust() -> Option<EngineTrustMarker> {
    ADOPTED
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}
