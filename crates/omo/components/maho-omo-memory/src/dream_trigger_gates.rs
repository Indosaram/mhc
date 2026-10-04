use std::path::Path;
use memory_core::reflection::machine::DreamOrigin;
pub const DREAM_VOLUME_GATE_BYTES:usize=8192;
#[derive(Clone,Debug)]
pub struct DreamTriggerSettings{pub enabled:bool,pub idle_minutes:f64,pub min_hours_between:f64,pub shutdown_launch:bool,pub auto_select_max:usize,pub auto_select_max_chars:usize}
impl Default for DreamTriggerSettings{fn default()->Self{Self{enabled:true,idle_minutes:30.0,min_hours_between:24.0,shutdown_launch:true,auto_select_max:5,auto_select_max_chars:150000}}}
pub fn resolve_dream_trigger_settings(settings:&serde_json::Value,agent:Option<&str>)->Result<DreamTriggerSettings,String>{
    let base=&settings["dream"];let overrides=agent.map(|agent|&settings["agents"][agent]["dream"]);
    let field=|name:&str|overrides.and_then(|value|value.get(name)).filter(|value|!value.is_null()).or_else(||base.get(name));
    Ok(DreamTriggerSettings{
        enabled:field("enabled").and_then(serde_json::Value::as_bool).ok_or("Missing dream enabled")?,
        idle_minutes:field("idle_minutes").and_then(serde_json::Value::as_f64).ok_or("Missing dream idle_minutes")?,
        min_hours_between:field("min_hours_between").and_then(serde_json::Value::as_f64).ok_or("Missing dream min_hours_between")?,
        shutdown_launch:field("shutdown_launch").and_then(serde_json::Value::as_bool).ok_or("Missing dream shutdown_launch")?,
        auto_select_max:usize::try_from(field("auto_select_max").and_then(serde_json::Value::as_u64).ok_or("Missing dream auto_select_max")?).map_err(|error|error.to_string())?,
        auto_select_max_chars:usize::try_from(field("auto_select_max_chars").and_then(serde_json::Value::as_u64).ok_or("Missing dream auto_select_max_chars")?).map_err(|error|error.to_string())?,
    })
}
#[derive(Debug,PartialEq,Eq)]
pub enum DreamGateRejection{Disabled,ShutdownDisabled,TooSoon,InsufficientVolume}
pub fn evaluate_dream_gates<E>(origin:DreamOrigin,settings:&DreamTriggerSettings,now_ms:f64,last_dream_at_ms:impl FnOnce()->Result<Option<f64>,E>,unreflected_bytes:impl FnOnce()->Result<usize,E>)->Result<Result<(),DreamGateRejection>,E>{
    if origin==DreamOrigin::Manual{return Ok(Ok(()));}
    if !settings.enabled{return Ok(Err(DreamGateRejection::Disabled));}
    if origin==DreamOrigin::Shutdown&&!settings.shutdown_launch{return Ok(Err(DreamGateRejection::ShutdownDisabled));}
    if let Some(last)=last_dream_at_ms()?&&now_ms-last<=settings.min_hours_between*3_600_000.0{return Ok(Err(DreamGateRejection::TooSoon));}
    if unreflected_bytes()?<=DREAM_VOLUME_GATE_BYTES{return Ok(Err(DreamGateRejection::InsufficientVolume));}
    Ok(Ok(()))
}
pub fn read_last_dream_at_ms(runtime:&Path)->std::io::Result<Option<f64>>{
    let bytes=match std::fs::read(runtime.join("dream/state.json")){Ok(bytes)=>bytes,Err(error) if error.kind()==std::io::ErrorKind::NotFound=>return Ok(None),Err(error)=>return Err(error)};
    let timestamp=serde_json::from_slice::<serde_json::Value>(&bytes).ok().and_then(|value|value.get("last_dream_at").and_then(serde_json::Value::as_str).map(str::to_owned)).and_then(|timestamp|chrono::DateTime::parse_from_rfc3339(&timestamp).ok()).map(|time|time.timestamp_millis().to_string().parse::<f64>().unwrap_or(f64::NAN));
    Ok(timestamp.filter(|timestamp|timestamp.is_finite()))
}
#[cfg(test)]
mod tests{
    use super::*;
    #[test]fn agent_dream_overrides_preserve_unmodified_base_fields(){
        let mut settings=crate::reflection_settings::resolve_memory_settings(None).unwrap();settings["agents"]=serde_json::json!({"agent":{"dream":{"enabled":false,"idle_minutes":0.5,"auto_select_max":2}}});
        let result=resolve_dream_trigger_settings(&settings,Some("agent")).unwrap();assert!(!result.enabled);assert_eq!(result.idle_minutes,0.5);assert_eq!(result.auto_select_max,2);assert_eq!(result.min_hours_between,24.0);assert!(result.shutdown_launch);assert_eq!(result.auto_select_max_chars,150000);
        assert!(resolve_dream_trigger_settings(&settings,Some("other")).unwrap().enabled);
    }
    #[test]fn manual_bypasses_all_probes(){let settings=DreamTriggerSettings{enabled:false,shutdown_launch:false,..Default::default()};assert_eq!(evaluate_dream_gates::<()>(DreamOrigin::Manual,&settings,0.0,||panic!("last"),||panic!("volume")),Ok(Ok(())));}
    #[test]fn early_rejections_are_lazy(){let settings=DreamTriggerSettings{enabled:false,..Default::default()};assert_eq!(evaluate_dream_gates::<()>(DreamOrigin::Idle,&settings,0.0,||panic!("last"),||panic!("volume")),Ok(Err(DreamGateRejection::Disabled)));let settings=DreamTriggerSettings{shutdown_launch:false,..Default::default()};assert_eq!(evaluate_dream_gates::<()>(DreamOrigin::Shutdown,&settings,0.0,||panic!("last"),||panic!("volume")),Ok(Err(DreamGateRejection::ShutdownDisabled)));}
    #[test]fn spacing_and_volume_are_strict(){let settings=DreamTriggerSettings::default();let spacing=settings.min_hours_between*3_600_000.0;assert_eq!(evaluate_dream_gates::<()>(DreamOrigin::Idle,&settings,spacing,||Ok(Some(0.0)),||panic!("volume")),Ok(Err(DreamGateRejection::TooSoon)));assert_eq!(evaluate_dream_gates::<()>(DreamOrigin::Idle,&settings,spacing+1.0,||Ok(Some(0.0)),||Ok(8192)),Ok(Err(DreamGateRejection::InsufficientVolume)));assert_eq!(evaluate_dream_gates::<()>(DreamOrigin::Shutdown,&settings,spacing+1.0,||Ok(Some(0.0)),||Ok(8193)),Ok(Ok(())));}
    #[test]fn errors_propagate_without_later_probe(){assert_eq!(evaluate_dream_gates(DreamOrigin::Idle,&DreamTriggerSettings::default(),0.0,||Err("io"),||panic!("volume")),Err("io"));}
    #[test]fn durable_state_missing_malformed_and_valid(){let dir=tempfile::tempdir().unwrap();assert_eq!(read_last_dream_at_ms(dir.path()).unwrap(),None);std::fs::create_dir(dir.path().join("dream")).unwrap();let path=dir.path().join("dream/state.json");for content in ["not json","[]",r#"{"last_dream_at":"invalid"}"#]{std::fs::write(&path,content).unwrap();assert_eq!(read_last_dream_at_ms(dir.path()).unwrap(),None);}std::fs::write(&path,r#"{"last_dream_at":"1970-01-01T00:00:01.000Z"}"#).unwrap();assert_eq!(read_last_dream_at_ms(dir.path()).unwrap(),Some(1000.0));}
}
