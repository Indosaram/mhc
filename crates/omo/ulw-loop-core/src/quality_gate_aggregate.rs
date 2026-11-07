//! Compatibility entry point for quality gate validation.

use serde_json::Value;

use crate::quality_gate::{ValidateQualityGateOptions, validate_quality_gate};
use crate::runtime::UlwLoopError;

pub fn aggregate_quality_gate_defects(
    input: &Value,
    opts: Option<&ValidateQualityGateOptions>,
) -> Result<(), UlwLoopError> {
    validate_quality_gate(input, opts)?;
    Ok(())
}
