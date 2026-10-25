//! Facts model selection deliberately has no built-in registry ladder.
use crate::worker::resolve_model::{
    ReflectionModelResolution, ReflectionSessionModel, resolve_reflection_model,
};
use senpi_task::host::{HostError, SenpiModelRegistry, parse_available_models};
use serde_json::Value;

pub fn resolve_facts_model(
    config: &Value,
    registry: Option<&dyn SenpiModelRegistry>,
    session: Option<&ReflectionSessionModel>,
    identity: &str,
) -> Result<ReflectionModelResolution, HostError> {
    let unavailable = |cause| ReflectionModelResolution::CategoryUnavailable {
        category: "quick".into(),
        cause,
        attempted_chain: None,
        missing_providers: None,
    };
    let patterns: Vec<String> = config["exclude_models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    let filtered = registry.map(|registry| {
        senpi_task::model_exclusion::without_excluded_models(registry, Some(&patterns))
    });
    let registry = filtered
        .as_ref()
        .map(|registry| registry as &dyn SenpiModelRegistry);
    let session = session.filter(|session| {
        !omo_config_core::models::is_excluded_model(
            &format!("{}/{}", session.provider, session.id),
            Some(&patterns),
        )
    });
    let configured = config["memory"]["agents"][identity]["facts"]["model"]
        .as_str()
        .or_else(|| config["memory"]["facts"]["model"].as_str());
    let quick = &config["categories"]["quick"];
    let available = match registry {
        Some(registry) => parse_available_models(&registry.get_available()?).unwrap_or_default(),
        None => Vec::new(),
    };
    if configured.is_none() && quick["disable"].as_bool() == Some(true) {
        return Ok(unavailable("not_found"));
    }
    if configured.is_none()
        && (quick["model"].is_string()
            || quick["models"]
                .as_array()
                .is_some_and(|models| !models.is_empty()))
    {
        let mut resolution = resolve_reflection_model("quick", config, registry, None)?;
        let mut declared = Vec::new();
        if let Some(model) = quick["model"].as_str() {
            declared.push(model.to_owned());
        }
        for key in ["models", "fallback_models"] {
            let entries = quick[key]
                .as_array()
                .cloned()
                .unwrap_or_else(|| vec![quick[key].clone()]);
            for entry in entries {
                if let Some(model) = entry.as_str().or_else(|| entry["model"].as_str()) {
                    declared.push(model.to_owned());
                }
            }
        }
        match &mut resolution {
            ReflectionModelResolution::Resolved {
                model,
                source: None,
                fallbacks,
                ..
            } if declared.contains(model) && available.contains(model) => {
                fallbacks.retain(|candidate| declared.contains(&candidate.model));
                return Ok(resolution);
            }
            _ => return Ok(unavailable("model_unavailable")),
        }
    }
    let inherited = session.map(|session| format!("{}/{}", session.provider, session.id));
    let selector = configured.or(inherited.as_deref());
    let Some(registry) = registry else {
        return Ok(unavailable("no_registry"));
    };
    let Some(selector) = selector else {
        return Ok(unavailable("not_found"));
    };
    let Some((provider, id)) = selector.split_once('/') else {
        return Ok(unavailable("model_unavailable"));
    };
    if provider.is_empty()
        || registry.find(provider, id).is_none()
        || !available.iter().any(|model| model == selector)
    {
        return Ok(unavailable("model_unavailable"));
    }
    Ok(ReflectionModelResolution::Resolved {
        category: "quick".into(),
        model: selector.into(),
        thinking: None,
        source: None,
        fallbacks: vec![],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    struct Registry;
    impl SenpiModelRegistry for Registry {
        fn get_available(&self) -> Result<Value, HostError> {
            Ok(json!([{"provider":"test","id":"allowed"},{"provider":"test","id":"excluded"}]))
        }
        fn find(&self, provider: &str, id: &str) -> Option<Value> {
            (provider == "test" && matches!(id, "allowed" | "excluded"))
                .then(|| json!({"provider":provider,"id":id}))
        }
    }
    #[test]
    fn excluded_explicit_and_inherited_facts_models_are_unavailable() {
        let config = json!({"exclude_models":["test/excluded"],"memory":{"facts":{"model":"test/excluded"}}});
        assert!(matches!(
            resolve_facts_model(&config, Some(&Registry), None, "agent").unwrap(),
            ReflectionModelResolution::CategoryUnavailable { .. }
        ));
        let session = ReflectionSessionModel {
            provider: "test".into(),
            id: "excluded".into(),
            thinking: None,
        };
        assert!(matches!(
            resolve_facts_model(
                &json!({"exclude_models":["test/excluded"]}),
                Some(&Registry),
                Some(&session),
                "agent"
            )
            .unwrap(),
            ReflectionModelResolution::CategoryUnavailable { .. }
        ));
    }
    #[test]
    fn explicit_facts_selection_has_no_builtin_fallbacks() {
        assert!(
            matches!(resolve_facts_model(&json!({"memory":{"facts":{"model":"test/allowed"}}}),Some(&Registry),None,"agent").unwrap(),ReflectionModelResolution::Resolved{fallbacks,..} if fallbacks.is_empty())
        );
    }
}
