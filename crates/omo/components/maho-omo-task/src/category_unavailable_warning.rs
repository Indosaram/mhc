use std::sync::{Arc, Mutex};
use serde_json::{json, Value};
use senpi_task::manager::types::{ChildPlanner, PlanResolutionCode};
use crate::usage_guidance::OncePerSessionGuard;

pub type WarningSink = Arc<dyn Fn(&str, Value) + Send + Sync>;
pub fn deliver_category_warning(actions: &dyn maho_ext_api::ExtensionActions, ui: Option<&dyn maho_ext_api::ExtensionUi>, text: &str, details: Value) -> Result<(), maho_ext_api::ExtensionFailure> {
    if let Some(ui) = ui { ui.notify(text, maho_ext_api::NotificationType::Warning); }
    actions.send_message(maho_ext_api::CustomMessage { custom_type: crate::renderers::CATEGORY_UNAVAILABLE_MESSAGE_TYPE.into(), content: vec![maho_ext_api::ToolContent::text(text)], display: true, details: Some(details) }, maho_ext_api::SendMessageOptions { trigger_turn: false, deliver_as: None })
}

pub fn create_category_unavailable_warning_planner(
    planner: ChildPlanner,
    config: Value,
    settings: Value,
    session_id: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    warn: WarningSink,
) -> ChildPlanner {
    let warned = Mutex::new(OncePerSessionGuard::default());
    Arc::new(move |spec| {
        let resolution = planner(spec);
        let Err(error) = &resolution else { return resolution };
        if error.code != PlanResolutionCode::ModelUnavailable { return resolution; }
        let (Some(category), Some(chain)) = (&error.category, &error.attempted_chain) else { return resolution };
        let category_config = &config["categories"][category];
        if !category_config["model"].is_null() { return resolution; }
        let enabled = category_config["warn_unavailable"].as_bool()
            .or_else(|| settings["warnings"]["unavailable_categories"].as_bool()).unwrap_or(true);
        if !enabled { return resolution; }
        let key = format!("{}:{category}", session_id().unwrap_or_else(|| "unknown-session".into()));
        if !warned.lock().unwrap_or_else(std::sync::PoisonError::into_inner).first_delivery(&key) {
            return resolution;
        }
        let providers = error.missing_providers.clone().unwrap_or_default();
        let chain: Vec<Value> = chain.iter().map(|entry| {
            let mut value = json!({"providers":entry.providers,"model":entry.model});
            if let Some(variant) = &entry.variant { value["variant"] = json!(variant); }
            value
        }).collect();
        let text = format!("Category \"{category}\" has no usable model: none of its fallback-chain providers are connected ({}).", providers.join(", "));
        warn(&text, json!({
            "category": category, "reason": "no_chain_rung_available",
            "attempted_chain": chain, "missing_providers": providers,
            "available_categories": error.available_categories.clone().unwrap_or_default(),
        }));
        resolution
    })
}
