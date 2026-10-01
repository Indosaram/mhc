use omo_config_core::{LoadOmoConfigOptions, LoadOmoConfigResult, ModelReferenceDiagnostic, OmoConfigDiagnostic, load_omo_config, resolve_model_references};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SenpiConfigDiagnostic { Config(OmoConfigDiagnostic), Model(ModelReferenceDiagnostic) }

pub struct SenpiOmoConfigResult {
    pub loaded: LoadOmoConfigResult,
    pub config: Value,
    pub diagnostics: Vec<SenpiConfigDiagnostic>,
}

pub fn load_senpi_omo_config(mut options: LoadOmoConfigOptions<'_>) -> SenpiOmoConfigResult {
    options.harness = Some("senpi".into());
    let loaded = load_omo_config(&options);
    let resolved = resolve_model_references(&Value::Object(loaded.config.clone()));
    let diagnostics = loaded.diagnostics.iter().cloned().map(SenpiConfigDiagnostic::Config)
        .chain(resolved.diagnostics.into_iter().map(SenpiConfigDiagnostic::Model)).collect();
    SenpiOmoConfigResult { loaded, config: resolved.view, diagnostics }
}
