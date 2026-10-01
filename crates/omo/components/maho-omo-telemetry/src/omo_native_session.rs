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
pub fn start_native_session(options:&crate::index::SenpiTelemetryOptions,session_id:&str,payload:&Value,agent_dir:&Path)->Result<Option<telemetry_core::EventTelemetryClient>,telemetry_core::TelemetryError> {
    use telemetry_core::*;
    use crate::product_identity::{create_omo_native_product_config,get_omo_native_state_dir,hash_session_id,EVENT_PROPERTY_ALLOWLISTS};
    let env=options.env.clone().unwrap_or_else(||std::env::vars().collect());let product=create_omo_native_product_config();
    if !is_telemetry_client_enabled(&TelemetryClientEnabledInput::for_product(Some(&env),&product)) {return Ok(None);}
    let os=options.os_provider.as_deref().unwrap_or_else(||get_default_telemetry_os_provider());
    let distinct_id=get_telemetry_distinct_id(&product.machine_id_prefix,os);
    let mut allowlists:EventPropertyAllowlist=EVENT_PROPERTY_ALLOWLISTS.iter().map(|(name,keys)|((*name).into(),keys.iter().map(|k|(*k).into()).collect())).collect();
    allowlists.insert("parallelism_summary".into(),crate::parallelism_schema::PARALLELISM_SUMMARY_SCHEMA.iter().map(|(k,_)|(*k).into()).collect());
    let client=create_event_telemetry_client(&CreateEventTelemetryClientInput {diagnostics:None,distinct_id:&distinct_id,env:Some(&env),on_capture:None,product:&product,property_allowlist:&allowlists,schema_version:1,set_timeout_fn:None,source:"omo-native-session",transport_factory:options.transport_factory.clone()});
    if !client.enabled() {return Ok(None);}
    let state=options.state_dir.clone().unwrap_or_else(||get_omo_native_state_dir(&env));
    let hash=hash_session_id(session_id,&state).map_err(|e|TelemetryError::new(e.to_string()))?;
    let activity=get_daily_active_capture_state(&DailyActiveCaptureStateInput {state_dir:&state,now:options.now,diagnostics:None});
    if activity.capture_daily {let properties=serde_json::Map::from_iter([("$session_id".into(),json!(hash)),("day_utc".into(),json!(activity.day_utc)),("reason".into(),json!("session_start"))]);client.capture_event("daily_active",&properties);}
    let mut inventory=read_inventory(agent_dir).unwrap_or_else(|error| {eprintln!("omo_native_inventory_read_failed: {error}");json!({"provider_count":0,"model_count":0,"providers":""})});
    let properties=inventory.as_object_mut().ok_or_else(||TelemetryError::new("Inventory must be an object"))?;
    for (key,value) in [("$session_id",json!(hash)),("$os",json!(os.platform())),("$os_version",json!(os.release())),("arch",json!(os.arch())),("cpu_count",json!(os.cpus()?.len())),("memory_bucket",json!(memory_bucket(os.totalmem()))),("reason",json!(session_reason(payload)))] {properties.insert(key.into(),value);}
    client.capture_event("session_started",properties);
    let mut options=options.clone();options.state_dir=Some(crate::index::get_senpi_telemetry_state_dir(&env));tokio::spawn(async move {if let Err(error)=crate::index::record_senpi_daily_active(&options).await {eprintln!("omo-senpi legacy telemetry failed: {error}");}});
    Ok(Some(client))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn inventory_masks_custom_names() {let t=tempfile::tempdir().unwrap();std::fs::write(t.path().join("models.json"),json!({"providers":{"openai":{"models":[{},{}]},"secret-provider":{"models":[{}]}}}).to_string()).unwrap();std::fs::write(t.path().join("settings.json"),json!({"defaultProvider":"secret-provider","defaultModel":"secret-model"}).to_string()).unwrap();let result=read_inventory(t.path()).unwrap();assert_eq!(result["provider_count"],2);assert_eq!(result["model_count"],3);assert_eq!(result["providers"],"openai");assert_eq!(result["default_model"],"custom");assert!(!result.to_string().contains("secret"));}
    #[test] fn invalid_inventory_reports_error() {let t=tempfile::tempdir().unwrap();assert!(read_inventory(t.path()).is_err());std::fs::write(t.path().join("models.json"),"[]").unwrap();assert!(read_inventory(t.path()).is_err());}
    #[test] fn memory_thresholds() {for (gib,expected) in [(0,"lt_8_gb"),(7,"lt_8_gb"),(8,"8_15_gb"),(16,"16_31_gb"),(32,"32_63_gb"),(64,"64_plus_gb")] {assert_eq!(memory_bucket(gib*1024*1024*1024),expected);}}
    #[test] fn reason_validation() {for reason in ["startup","reload","new","resume","fork"] {assert_eq!(session_reason(&json!({"reason":reason})),reason);}assert_eq!(session_reason(&json!({"reason":"secret"})),"startup");}
}
