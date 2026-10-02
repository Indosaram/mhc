//! Port of interactive/risky-main-model-warning.ts.

use maho_ai::types::Model;

pub const RISKY_MAIN_MODEL_WARNING: &str = "Not a recommended model. It can perform dangerous actions such as harming your computer and has not been tested. Please use a different model.";

pub fn is_risky_main_model(model: &Model) -> bool {
    let label = format!("{}/{} {}", model.provider, model.id, model.name).to_lowercase();
    ["minimax", "qwen"].iter().any(|family| label.contains(family))
}
