use super::{model_list::{build_wire_model, build_model_list_response}, registry::{JsonRpcError, MethodRegistration, MethodRegistry, MethodScope}};
use maho_ai::model::Model;
use serde_json::{Value, json};
use std::sync::Arc;

pub fn native_wire_model(model: &Model) -> Result<Value, JsonRpcError> {
    let levels = maho_core::thinking_levels::get_supported_thinking_levels(model).iter().map(|level| level.as_str().to_owned()).collect::<Vec<_>>();
    let default = maho_core::model_resolver::DEFAULT_MODEL_PER_PROVIDER.iter().find(|(provider,_)| *provider == model.provider).map(|(_,id)| *id);
    let model = serde_json::to_value(model).map_err(|error| JsonRpcError::new(-32603,error.to_string()))?;
    Ok(build_wire_model(&model,&levels,default))
}
pub fn register_model_list_method(registry: &mut MethodRegistry, models: Arc<dyn Fn() -> Vec<Model> + Send + Sync>) {
    registry.register("model/list".into(), MethodRegistration { requires_init:true, experimental:false, scope:MethodScope::None, handler:Arc::new(move |context| {
        let models = models.clone(); Box::pin(async move {
            let params = if context.request["params"].is_object() { context.request["params"].clone() } else { json!({}) };
            for (key, valid) in [("cursor",params["cursor"].is_string()),("includeHidden",params["includeHidden"].is_boolean()),("limit",params["limit"].as_f64().is_some_and(|value| value >= 0.0 && value <= f64::from(u32::MAX) && value.fract() == 0.0))] {
                if params.get(key).is_some_and(|value| !value.is_null()) && !valid { return Err(JsonRpcError::new(-32600,format!("model/list received an invalid {key}"))); }
            }
            let wire = models().iter().map(native_wire_model).collect::<Result<Vec<_>,_>>()?;
            build_model_list_response(&wire,&params)
        })
    }) });
    registry.register("remoteControl/client/list".into(), MethodRegistration { requires_init:true, experimental:true, scope:MethodScope::None, handler:Arc::new(|context| Box::pin(async move {
        let params = &context.request["params"];
        if !params.is_object() { return Err(JsonRpcError::new(-32600,"remoteControl/client/list requires params")); }
        if !params["environmentId"].is_string() { return Err(JsonRpcError::new(-32600,"remoteControl/client/list requires a string environmentId")); }
        for (key, valid) in [("cursor",params["cursor"].is_string()),("limit",params["limit"].as_f64().is_some_and(|value| value >= 0.0 && value <= f64::from(u32::MAX) && value.fract() == 0.0)),("order",matches!(params["order"].as_str(),Some("asc"|"desc")))] {
            if params.get(key).is_some_and(|value| !value.is_null()) && !valid { return Err(JsonRpcError::new(-32600,format!("remoteControl/client/list received an invalid {key}"))); }
        }
        Err(JsonRpcError::new(-32603,"remote control is unavailable for this app-server"))
    })) });
}
