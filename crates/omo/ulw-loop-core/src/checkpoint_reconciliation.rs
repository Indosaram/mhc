//! Checkpoint error combination.

use std::collections::BTreeMap;
use crate::runtime::UlwLoopError;

pub fn combine_checkpoint_validation_errors(codex_error: &UlwLoopError, gate_error: &UlwLoopError) -> UlwLoopError {
    let mut details = BTreeMap::new();
    if let Some(d) = &codex_error.details {
        details.extend(d.clone());
    }
    if let Some(d) = &gate_error.details {
        details.extend(d.clone());
    }
    UlwLoopError::with_details(
        format!("{}\n{}", codex_error.message, gate_error.message),
        "ULW_LOOP_QUALITY_GATE_INVALID",
        details,
    )
}
