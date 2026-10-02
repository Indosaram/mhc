use serde_json::Value;
use maho_ext_api::ServiceTier;
pub const CODEX_RESPONSES_API:&str="openai-codex-responses";
pub fn supports_service_tier(api:Option<&str>)->bool{matches!(api,Some("openai-responses"|CODEX_RESPONSES_API))}
pub fn add_service_tier_to_payload(api:Option<&str>,mut payload:Value,tier:Option<ServiceTier>)->Value{
    if supports_service_tier(api)&&let Some(tier)=tier&&let Some(object)=payload.as_object_mut()&&!object.contains_key("service_tier"){
        object.insert("service_tier".into(),Value::String(match tier{ServiceTier::Auto=>"auto",ServiceTier::Flex=>"flex",ServiceTier::Priority=>"priority"}.into()));
    }
    payload
}
pub fn effective_service_tier(api:Option<&str>,session_fast_mode:bool,context_tier:Option<ServiceTier>,settings_tier:Option<ServiceTier>,live_memory_tier:Option<ServiceTier>,same_base_key:bool,catalog_explains_priority:bool)->Option<ServiceTier>{
    if api==Some(CODEX_RESPONSES_API){
        if session_fast_mode{Some(ServiceTier::Priority)}else if live_memory_tier==Some(ServiceTier::Auto)&&same_base_key&&context_tier==Some(ServiceTier::Priority)&&catalog_explains_priority{None}else{context_tier}
    }else{context_tier.or(settings_tier)}
}
