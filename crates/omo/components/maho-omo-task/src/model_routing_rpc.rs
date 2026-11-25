//! `model-routing-rpc.ts`: `omo.modelRouting.resolve` RPC handler for desktop model routing inspection.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use maho_ext_api::{ExtensionApi, ExtensionFailure};
use maho_omo_config_resolution::{SenpiConfigDiagnostic, SenpiOmoConfigResult};

use crate::{
    model_routing_rows::{ModelRoutingPlannerInputs, ModelRoutingRow, resolve_model_routing_rows},
    planner::ResolveModelRegistry,
};

/// The desktop app's model-routing read. `schemaVersion` versions the answer.
pub const MODEL_ROUTING_RESOLVE_RPC_METHOD: &str = "omo.modelRouting.resolve";

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelRoutingErrorCode {
    CwdUnavailable,
    ConfigInvalid,
    RegistryUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum ModelRoutingAnswer {
    Ok {
        #[serde(rename = "schemaVersion")]
        schema_version: u32,
        rows: Vec<ModelRoutingRow>,
    },
    Error {
        kind: &'static str,
        code: ModelRoutingErrorCode,
        message: String,
    },
}

pub struct ModelRoutingRpcDeps {
    /// The directory the planner's live config is read from (the task component's register-time cwd).
    pub cwd: Arc<dyn Fn() -> PathBuf + Send + Sync>,
    pub load_config: Arc<dyn Fn(&Path) -> SenpiOmoConfigResult + Send + Sync>,
    pub registry: ResolveModelRegistry,
    /// The planner's own agent table and the parent's service tier, read per answer.
    pub planner: Arc<dyn Fn() -> ModelRoutingPlannerInputs + Send + Sync>,
}

// A layer the loader could not read or parse is skipped, so rows built without it would describe a
// config the user never wrote; pruned values and unknown keys keep the rest of the layer and still
// resolve as the engine does.
fn is_unreadable_config_kind(kind: &str) -> bool {
    matches!(kind, "read" | "parse" | "validation")
}

fn diagnostic_kind_and_message(diagnostic: &SenpiConfigDiagnostic) -> (&str, &str) {
    match diagnostic {
        SenpiConfigDiagnostic::Config(config) => (config.kind, config.message.as_str()),
        SenpiConfigDiagnostic::Model(model) => (model.kind, model.message.as_str()),
    }
}

pub fn register_model_routing_rpc(
    api: &mut ExtensionApi,
    deps: ModelRoutingRpcDeps,
) -> Result<(), ExtensionFailure> {
    let deps = Arc::new(deps);
    api.rpc_handle(
        MODEL_ROUTING_RESOLVE_RPC_METHOD,
        Arc::new(move |_params| {
            let deps = deps.clone();
            Box::pin(async move {
                serde_json::to_value(answer_model_routing(&deps))
                    .map_err(|error| ExtensionFailure::new(error.to_string()))
            })
        }),
    )
}

pub fn answer_model_routing(deps: &ModelRoutingRpcDeps) -> ModelRoutingAnswer {
    let cwd = (deps.cwd)();
    if !cwd.is_dir() {
        return ModelRoutingAnswer::Error {
            kind: "error",
            code: ModelRoutingErrorCode::CwdUnavailable,
            message: format!(
                "The session's working directory {} is not a directory.",
                cwd.display()
            ),
        };
    }

    let loaded = (deps.load_config)(&cwd);
    let unreadable: Vec<&str> = loaded
        .diagnostics
        .iter()
        .filter_map(|diagnostic| {
            let (kind, message) = diagnostic_kind_and_message(diagnostic);
            is_unreadable_config_kind(kind).then_some(message)
        })
        .collect();
    if !unreadable.is_empty() {
        return ModelRoutingAnswer::Error {
            kind: "error",
            code: ModelRoutingErrorCode::ConfigInvalid,
            message: unreadable.join("\n"),
        };
    }

    let Some(registry) = (deps.registry)() else {
        return ModelRoutingAnswer::Error {
            kind: "error",
            code: ModelRoutingErrorCode::RegistryUnavailable,
            message: "The session has no model registry yet.".to_string(),
        };
    };

    let planner = (deps.planner)();
    ModelRoutingAnswer::Ok {
        schema_version: 1,
        rows: resolve_model_routing_rows(&loaded.config, registry.as_ref(), &planner),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omo_config_core::{LoadOmoConfigResult, OmoConfigDiagnostic};
    use senpi_task::host::{HostError, SenpiModelRegistry};
    use serde_json::{Value, json};

    struct EmptyRegistry;

    impl SenpiModelRegistry for EmptyRegistry {
        fn get_available(&self) -> Result<Value, HostError> {
            Ok(Value::Array(Vec::new()))
        }
        fn find(&self, _provider: &str, _model_id: &str) -> Option<Value> {
            None
        }
    }

    fn ok_result(diagnostics: Vec<SenpiConfigDiagnostic>) -> SenpiOmoConfigResult {
        SenpiOmoConfigResult {
            loaded: LoadOmoConfigResult {
                config: Default::default(),
                diagnostics: Vec::new(),
                layers: Vec::new(),
                profile: None,
                sources: Vec::new(),
            },
            config: json!({}),
            diagnostics,
        }
    }

    #[test]
    fn returns_cwd_unavailable_when_cwd_is_not_a_directory() {
        let missing = PathBuf::from("/nonexistent-mhc-p10b-model-routing-dir");
        let answer = answer_model_routing(&ModelRoutingRpcDeps {
            cwd: Arc::new(move || missing.clone()),
            load_config: Arc::new(|_| ok_result(Vec::new())),
            registry: Arc::new(|| None),
            planner: Arc::new(ModelRoutingPlannerInputs::default),
        });
        assert!(matches!(
            answer,
            ModelRoutingAnswer::Error {
                code: ModelRoutingErrorCode::CwdUnavailable,
                ..
            }
        ));
    }

    #[test]
    fn returns_config_invalid_for_parse_or_read_or_validation_but_not_pruned_warnings() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dir_path = dir.path().to_path_buf();
        let invalid = answer_model_routing(&ModelRoutingRpcDeps {
            cwd: Arc::new(move || dir_path.clone()),
            load_config: Arc::new(|_| {
                ok_result(vec![SenpiConfigDiagnostic::Config(OmoConfigDiagnostic {
                    kind: "parse",
                    path: "omo.json".into(),
                    message: "bad json".into(),
                    issue_paths: Vec::new(),
                })])
            }),
            registry: Arc::new(|| Some(Arc::new(EmptyRegistry))),
            planner: Arc::new(ModelRoutingPlannerInputs::default),
        });
        assert!(matches!(
            invalid,
            ModelRoutingAnswer::Error {
                code: ModelRoutingErrorCode::ConfigInvalid,
                message,
                ..
            } if message == "bad json"
        ));

        let dir_path = dir.path().to_path_buf();
        let pruned_only = answer_model_routing(&ModelRoutingRpcDeps {
            cwd: Arc::new(move || dir_path.clone()),
            load_config: Arc::new(|_| {
                ok_result(vec![SenpiConfigDiagnostic::Config(OmoConfigDiagnostic {
                    kind: "unknown-keys",
                    path: "omo.json".into(),
                    message: "ignored unknown key".into(),
                    issue_paths: Vec::new(),
                })])
            }),
            registry: Arc::new(|| Some(Arc::new(EmptyRegistry))),
            planner: Arc::new(ModelRoutingPlannerInputs::default),
        });
        assert!(matches!(
            pruned_only,
            ModelRoutingAnswer::Ok {
                schema_version: 1,
                ..
            }
        ));
    }

    #[test]
    fn returns_registry_unavailable_when_registry_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dir_path = dir.path().to_path_buf();
        let answer = answer_model_routing(&ModelRoutingRpcDeps {
            cwd: Arc::new(move || dir_path.clone()),
            load_config: Arc::new(|_| ok_result(Vec::new())),
            registry: Arc::new(|| None),
            planner: Arc::new(ModelRoutingPlannerInputs::default),
        });
        assert!(matches!(
            answer,
            ModelRoutingAnswer::Error {
                code: ModelRoutingErrorCode::RegistryUnavailable,
                ..
            }
        ));
    }
}
