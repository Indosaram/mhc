#[derive(Clone,Debug,PartialEq,Eq)]
pub struct ReflectionSessionModel {pub provider:String,pub id:String}
pub fn resolve_memory_session_model(context:&serde_json::Value)->Option<ReflectionSessionModel>{
    let model=context.as_object()?.get("model")?.as_object()?;
    let provider=model.get("provider")?.as_str().filter(|provider|!provider.is_empty())?;
    let id=model.get("id")?.as_str().filter(|id|!id.is_empty())?;
    Some(ReflectionSessionModel{provider:provider.into(),id:id.into()})
}
pub fn resolve_native_memory_session_model(context:&maho_ext_api::ExtensionContext)->Option<ReflectionSessionModel>{
    let model=context.model.as_ref()?;if model.provider.is_empty()||model.id.is_empty(){return None;}
    Some(ReflectionSessionModel{provider:model.provider.clone(),id:model.id.clone()})
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]fn resolves_provider_and_id_only(){assert_eq!(resolve_memory_session_model(&serde_json::json!({"model":{"provider":"anthropic","id":"claude-opus-5","name":"name","contextWindow":200000}})),Some(ReflectionSessionModel{provider:"anthropic".into(),id:"claude-opus-5".into()}));}
    #[test]fn malformed_context_or_model_is_absent(){for value in [serde_json::Value::Null,serde_json::json!([]),serde_json::json!("nope"),serde_json::json!({}),serde_json::json!({"model":null}),serde_json::json!({"model":{"provider":"anthropic"}}),serde_json::json!({"model":{"id":"model"}}),serde_json::json!({"model":{"provider":"","id":"model"}}),serde_json::json!({"model":{"provider":"provider","id":""}}),serde_json::json!({"model":{"provider":1,"id":2}})]{assert_eq!(resolve_memory_session_model(&value),None);}}
}
