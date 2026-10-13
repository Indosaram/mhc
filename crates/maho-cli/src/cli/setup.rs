//! Native equivalent of the API registration triggered by the upstream CLI imports.
pub fn register_builtin_apis() {
    maho_ai::compat::register_builtin_api_providers();
}
