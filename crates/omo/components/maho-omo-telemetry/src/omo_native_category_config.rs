use senpi_task::{
    category::{CategoryResolutionResult, ResolveCategoryOptions, resolve_category},
    host::{HostError, SenpiModelRegistry},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

struct Registry(std::sync::Arc<dyn maho_ext_api::ModelRegistry>);
impl SenpiModelRegistry for Registry {
    fn get_available(&self) -> Result<Value, HostError> {
        Ok(json!(
            self.0
                .get_available()
                .iter()
                .map(|m| json!({"provider":m.provider,"id":m.id,"name":m.name}))
                .collect::<Vec<_>>()
        ))
    }
    fn find(&self, provider: &str, id: &str) -> Option<Value> {
        self.0
            .find(provider, id)
            .map(|m| json!({"provider":m.provider,"id":m.id,"name":m.name}))
    }
}

#[derive(Default)]
pub struct CategoryConfigCapture {
    canonical: Option<String>,
    generation: u64,
}
impl CategoryConfigCapture {
    pub fn observe(
        &mut self,
        ctx: &maho_ext_api::ExtensionContext,
        env: &telemetry_core::TelemetryEnv,
        hash: &str,
        source: &str,
    ) -> Result<Option<Value>, String> {
        let loaded = maho_omo_config_resolution::load_senpi_omo_config(
            omo_config_core::LoadOmoConfigOptions {
                cwd: Some(ctx.cwd.to_string_lossy().into_owned()),
                env: Some(env.iter().map(|(k, v)| (k.clone(), v.clone())).collect()),
                ..Default::default()
            },
        );
        if !loaded.diagnostics.is_empty() {
            return Err(format!("{:?}", loaded.diagnostics));
        }
        let config = serde_json::to_value(&loaded.config).map_err(|e| e.to_string())?;
        let registry = Registry(ctx.current_model_registry().map_err(|e| e.to_string())?);
        let names = crate::product_identity::builtin_category_names();
        let configured = config["categories"].as_object();
        let user = configured.map_or(0, |c| {
            c.keys().filter(|n| !names.contains(&n.as_str())).count()
        });
        let overridden = configured.map_or(0, |c| {
            c.keys().filter(|n| names.contains(&n.as_str())).count()
        });
        let mut categories = std::collections::BTreeMap::new();
        for name in names {
            let value = match resolve_category(
                name,
                &config,
                &registry,
                &ResolveCategoryOptions::default(),
            ) {
                Ok(CategoryResolutionResult::Resolved {
                    spec,
                    model_selection,
                    ..
                }) => {
                    let (provider, model) = crate::product_identity::mask_provider_and_model(
                        &spec.provider,
                        &spec.model_id,
                    );
                    let reasoning = spec
                        .reasoning
                        .as_deref()
                        .or(spec.reasoning_effort.as_deref())
                        .or(spec.variant.as_deref())
                        .unwrap_or("none");
                    let reasoning = if crate::delegation_schema::DELEGATION_REASONING_EFFORTS
                        .contains(&reasoning)
                    {
                        reasoning
                    } else {
                        "other"
                    };
                    let depth = model_selection
                        .fallback_entry
                        .as_ref()
                        .and_then(|entry| {
                            senpi_task::category::category_fallback_chain(name).and_then(|chain| {
                                chain.iter().position(|rung| rung.model == entry.model)
                            })
                        })
                        .unwrap_or(0);
                    format!(
                        "{provider}/{model}/{reasoning}{}",
                        if depth == 0 {
                            String::new()
                        } else {
                            format!("+{depth}")
                        }
                    )
                }
                Ok(CategoryResolutionResult::Disabled { .. }) => "disabled".into(),
                Ok(CategoryResolutionResult::NotFound { .. }) => "absent".into(),
                Ok(CategoryResolutionResult::ModelUnavailable(_)) | Err(_) => "unavailable".into(),
            };
            categories.insert(name, value);
        }
        let canonical = format!(
            "{}|u={user}|o={overridden}",
            categories
                .iter()
                .map(|(name, value)| format!("{name}={value};"))
                .collect::<String>()
        );
        if self.canonical.as_ref() == Some(&canonical) {
            return Ok(None);
        }
        if self.canonical.is_some() {
            self.generation += 1;
        }
        let fingerprint = format!("{:x}", Sha256::digest(canonical.as_bytes()));
        self.canonical = Some(canonical);
        let mut properties = json!({"$session_id":hash,"config_generation":self.generation,"combo_fingerprint":&fingerprint[..16],"user_category_count":user,"builtin_overridden_count":overridden,"source":source});
        for (name, value) in categories {
            properties[crate::category_config_schema::category_property_name(name)] = json!(value);
        }
        Ok(Some(properties))
    }
}
