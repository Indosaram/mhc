use serde_json::{Value, json};

pub fn requested_approval_policy(params: &Value) -> Value {
    let policy = &params["approvalPolicy"];
    match policy.as_str() {
        Some("untrusted" | "on-request" | "never") => policy.clone(),
        _ if ["sandbox_approval", "rules", "skill_approval", "request_permissions", "mcp_elicitations"].iter().all(|key| policy["granular"][key].is_boolean()) => policy.clone(),
        _ => json!("never"),
    }
}
pub fn parse_model_reference<'a>(model: &'a str, model_provider: Option<&'a str>) -> Option<(&'a str, &'a str)> {
    if let Some(provider) = model_provider.filter(|provider| !provider.is_empty()) {
        let id = model.strip_prefix(provider).and_then(|suffix| suffix.strip_prefix('/')).unwrap_or(model);
        return Some((provider, id));
    }
    model.split_once('/').filter(|(provider, id)| !provider.is_empty() && !id.is_empty())
}
pub fn requested_start_model<'a>(params: &Value, builtins: &'a [Value]) -> Option<&'a Value> {
    let model = params["model"].as_str().filter(|model| !model.is_empty())?;
    let (provider, id) = parse_model_reference(model, params["modelProvider"].as_str())?;
    builtins.iter().find(|model| model["provider"] == provider && model["id"] == id)
}
