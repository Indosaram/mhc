use std::path::Path;
use serde_json::{Value,json};
use crate::product_identity::{KNOWN_MODELS,mask_provider_and_model};
pub fn read_inventory(agent_dir:&Path) -> Result<Value,String> {
    let read=|name:&str|->Result<Value,String> {let raw=std::fs::read_to_string(agent_dir.join(name)).map_err(|e|e.to_string())?;let v:Value=serde_json::from_str(&raw).map_err(|e|e.to_string())?;if !v.is_object() {return Err(format!("{name} must contain an object"));}Ok(v)};
    let models=read("models.json")?;let settings=read("settings.json")?;
    let providers=models.get("providers").and_then(Value::as_object).ok_or("models.json providers must be an object")?;
    let mut model_count=0;
    for provider in providers.values() {
        if !provider.is_object() {return Err("models.json provider must be an object".into());}
        if let Some(models)=provider.get("models") {model_count+=models.as_array().ok_or("models.json provider models must be an array")?.len();}
    }
    let mut ids:Vec<_>=providers.keys().filter(|id|KNOWN_MODELS.iter().any(|(p,_)|p==id)).cloned().collect();ids.sort();
    let mut result=json!({"provider_count":providers.len(),"model_count":model_count,"providers":ids.join(",")});
    if let (Some(provider),Some(model))=(settings.get("defaultProvider").and_then(Value::as_str).filter(|s|!s.is_empty()),settings.get("defaultModel").and_then(Value::as_str).filter(|s|!s.is_empty())) {let (p,m)=mask_provider_and_model(provider,model);result["default_provider"]=json!(p);result["default_model"]=json!(m);}
    Ok(result)
}
pub fn session_reason(payload:&Value)->&str {match payload.get("reason").and_then(Value::as_str) {Some(reason @ ("startup"|"reload"|"new"|"resume"|"fork"))=>reason,_=>"startup"}}
pub fn memory_bucket(bytes:u64)->&'static str {let gib=bytes/1024/1024/1024;match gib {0..8=>"lt_8_gb",8..16=>"8_15_gb",16..32=>"16_31_gb",32..64=>"32_63_gb",_=>"64_plus_gb"}}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn inventory_masks_custom_names() {let t=tempfile::tempdir().unwrap();std::fs::write(t.path().join("models.json"),json!({"providers":{"openai":{"models":[{},{}]},"secret-provider":{"models":[{}]}}}).to_string()).unwrap();std::fs::write(t.path().join("settings.json"),json!({"defaultProvider":"secret-provider","defaultModel":"secret-model"}).to_string()).unwrap();let result=read_inventory(t.path()).unwrap();assert_eq!(result["provider_count"],2);assert_eq!(result["model_count"],3);assert_eq!(result["providers"],"openai");assert_eq!(result["default_model"],"custom");assert!(!result.to_string().contains("secret"));}
    #[test] fn invalid_inventory_reports_error() {let t=tempfile::tempdir().unwrap();assert!(read_inventory(t.path()).is_err());std::fs::write(t.path().join("models.json"),"[]").unwrap();assert!(read_inventory(t.path()).is_err());}
    #[test] fn memory_thresholds() {for (gib,expected) in [(0,"lt_8_gb"),(7,"lt_8_gb"),(8,"8_15_gb"),(16,"16_31_gb"),(32,"32_63_gb"),(64,"64_plus_gb")] {assert_eq!(memory_bucket(gib*1024*1024*1024),expected);}}
    #[test] fn reason_validation() {for reason in ["startup","reload","new","resume","fork"] {assert_eq!(session_reason(&json!({"reason":reason})),reason);}assert_eq!(session_reason(&json!({"reason":"secret"})),"startup");}
}
