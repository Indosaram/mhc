use serde_json::{Value,json};
use crate::product_identity::mask_provider_and_model;
pub fn turn_completed_properties(event:&Value,session_id:&str) -> Option<(Value,bool)> {
    let message=event.get("message")?;
    if message.get("role").and_then(Value::as_str)!=Some("assistant") {return None;}
    let usage=message.get("usage");
    let number=|key:&str,optional:bool| {let value=usage.and_then(|u|u.get(key));if optional && value.is_none() {return (0.0,true);}match value.and_then(Value::as_f64).filter(|n|n.is_finite() && *n>=0.0) {Some(n)=>(n,true),None=>(0.0,false)}};
    let input=number("input",false);let output=number("output",false);let read=number("cacheRead",false);let write=number("cacheWrite",false);let reasoning=number("reasoning",true);let total=number("totalTokens",false);
    let cost=usage.and_then(|u|u.get("cost")).and_then(|c|c.get("total")).and_then(Value::as_f64).filter(|n|n.is_finite() && *n>=0.0);
    let (provider,model)=mask_provider_and_model(message.get("provider").and_then(Value::as_str).unwrap_or_default(),message.get("model").and_then(Value::as_str).unwrap_or_default());
    let invalid=![input,output,read,write,reasoning,total].iter().all(|n|n.1) || cost.is_none();
    Some((json!({"$session_id":session_id,"provider":provider,"model_id":model,"input_tokens":input.0,"output_tokens":output.0,"cache_read_tokens":read.0,"cache_write_tokens":write.0,"reasoning_tokens":reasoning.0,"total_tokens":total.0,"cost_usd":((cost.unwrap_or(0.0)+f64::EPSILON)*10000.0).round()/10000.0,"turn_index":event.get("turnIndex")}),invalid))
}
pub fn register_omo_native_turn_telemetry(api:&mut maho_ext_api::ExtensionApi,hash:std::sync::Arc<dyn Fn(&str)->String+Send+Sync>,capture:crate::omo_native_parallel_summary::SummaryCapture,diagnostic:std::sync::Arc<dyn Fn()+Send+Sync>) {
    use maho_ext_api::{EventKind,EventResult,ExtensionEvent};
    api.on(EventKind::TurnEnd,std::sync::Arc::new(move |event,ctx| {let output=if let ExtensionEvent::TurnEnd {turn_index,message,..}=event {turn_completed_properties(&json!({"turnIndex":turn_index,"message":message}),&hash(ctx.session_manager.session_id()))} else {None};let capture=std::sync::Arc::clone(&capture);let diagnostic=std::sync::Arc::clone(&diagnostic);Box::pin(async move {if let Some((properties,invalid))=output {if invalid {diagnostic();}capture("turn_completed",properties);}Ok(EventResult::None)})}));
}
#[cfg(test)]
mod tests {
    use super::*;
    fn event()->Value {json!({"turnIndex":7,"message":{"role":"assistant","provider":"openai","model":"gpt-5.6-sol","usage":{"input":11,"output":22,"cacheRead":3,"cacheWrite":4,"reasoning":5,"totalTokens":40,"cost":{"total":0.123456}}}})}
    #[test] fn model_mask_independent() {assert_eq!(mask_provider_and_model("openai","gpt-5.6-sol"),("openai".into(),"gpt-5.6-sol".into()));assert_eq!(mask_provider_and_model("openai","user-defined-model"),("openai".into(),"custom".into()));assert_eq!(mask_provider_and_model("sionic-openrouter","gpt-5.6-sol"),("custom".into(),"custom".into()));assert_eq!(mask_provider_and_model("sionic-openrouter","user-defined-model"),("custom".into(),"custom".into()));}
    #[test] fn complete_payload() {let (p,invalid)=turn_completed_properties(&event(),"hashed-session").unwrap();assert!(!invalid);assert_eq!(p,json!({"$session_id":"hashed-session","provider":"openai","model_id":"gpt-5.6-sol","input_tokens":11.0,"output_tokens":22.0,"cache_read_tokens":3.0,"cache_write_tokens":4.0,"reasoning_tokens":5.0,"total_tokens":40.0,"cost_usd":0.1235,"turn_index":7}));}
    #[test] fn absent_reasoning_zero() {let mut e=event();e["message"]["usage"].as_object_mut().unwrap().remove("reasoning");let (p,invalid)=turn_completed_properties(&e,"s").unwrap();assert_eq!(p["reasoning_tokens"],0.0);assert_eq!(p["total_tokens"],40.0);assert!(!invalid);}
    #[test] fn non_assistant_ignored() {assert!(turn_completed_properties(&json!({"message":{"role":"user"}}),"s").is_none());}
    #[test] fn missing_usage_zero_diagnostic() {let (p,invalid)=turn_completed_properties(&json!({"message":{"role":"assistant"}}),"s").unwrap();assert!(invalid);for key in ["input_tokens","output_tokens","cache_read_tokens","cache_write_tokens","reasoning_tokens","total_tokens","cost_usd"] {assert_eq!(p[key],0.0);}}
    #[test] fn malformed_usage_sanitized() {let mut e=event();e["message"]["usage"]=json!({"input":null,"output":-2,"cacheRead":3,"cacheWrite":null,"reasoning":-1,"totalTokens":4,"cost":null});let (p,invalid)=turn_completed_properties(&e,"s").unwrap();assert!(invalid);assert_eq!(p["input_tokens"],0.0);assert_eq!(p["output_tokens"],0.0);assert_eq!(p["cache_read_tokens"],3.0);assert_eq!(p["total_tokens"],4.0);}
}
