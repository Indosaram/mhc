use serde_json::{Map,Value,json};
pub fn setting_sources(settings:&crate::settings::ProviderSettings,mode:&str,lane:crate::auth_lane::TokenInjection)->Value {
    settings.values.get("settingSources").cloned().unwrap_or_else(||if mode=="preset-append"&&lane==crate::auth_lane::TokenInjection::Ambient {json!(["user","project"])}else {json!([])})
}
pub fn tool_options(settings:&crate::settings::ProviderSettings,tool_less:bool,empty_tools:bool,tools:Option<&[String]>)->Map<String,Value> {
    let mut options=Map::new();let selected=if tool_less||empty_tools {Vec::new()}else {tools.map(<[String]>::to_vec).unwrap_or_else(||["Read","Write","Edit","Bash","Grep","Glob"].map(str::to_owned).to_vec())};options.insert("tools".into(),json!(selected));
    let append=settings.values.get("appendSystemPrompt")!=Some(&Value::Bool(false));let strict=tool_less||settings.values.get("strictMcpConfig").and_then(Value::as_bool).unwrap_or(!append);if tool_less {options.insert("maxTurns".into(),json!(1));}
    if strict {options.insert("extraArgs".into(),json!({"strict-mcp-config":null}));}options
}
pub fn thinking_options(model:&Value,reasoning:Option<&str>,budgets:Option<&Value>)->Map<String,Value> {
    let mut result=Map::new();let Some(reasoning)=reasoning else {return result;};let id=model["id"].as_str().unwrap_or("").to_lowercase();
    let includes=|markers:&[&str]|markers.iter().any(|marker|id.contains(marker));
    let adaptive=model["compat"]["forceAdaptiveThinking"].as_bool().unwrap_or_else(||includes(&["opus-4-6","opus-4.6","opus-4-7","opus-4.7","opus-4-8","opus-4.8","opus-5","sonnet-4-6","sonnet-4.6","sonnet-5","fable-5","mythos-5"]));
    if adaptive {
        result.insert("thinking".into(),json!({"type":"adaptive","display":"summarized"}));let mapped=model["thinkingLevelMap"][reasoning].as_str().filter(|level|["low","medium","high","xhigh","max"].contains(level));
        let effort=mapped.unwrap_or_else(||match reasoning {"minimal"|"low"=>"low","medium"=>"medium","high"=>"high","xhigh" if includes(&["opus-4-7","opus-4-8","opus-5","sonnet-5","fable-5","mythos-5"])=>"xhigh",_=>"max"});result.insert("effort".into(),json!(effort));
    }else {
        let level=if reasoning=="xhigh" {"high"}else {reasoning};let custom=budgets.and_then(|value|value[level].as_f64()).filter(|value|value.is_finite()&&*value>0.0);let default=match level {"minimal"=>2048.0,"low"=>8192.0,"medium"=>16384.0,"high"=>31999.0,_=>63999.0};result.insert("maxThinkingTokens".into(),json!(custom.unwrap_or(default)));
    }result
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn adaptive_markers_force_override_and_native_effort_mapping() {
        assert_eq!(thinking_options(&json!({"id":"claude-opus-4-6"}),Some("xhigh"),None)["effort"],"max");assert_eq!(thinking_options(&json!({"id":"claude-opus-5-5"}),Some("xhigh"),None)["effort"],"xhigh");assert_eq!(thinking_options(&json!({"id":"claude-opus-5-5","thinkingLevelMap":{"high":"low"}}),Some("high"),None)["effort"],"low");assert_eq!(thinking_options(&json!({"id":"claude-opus-5-5","compat":{"forceAdaptiveThinking":false}}),Some("xhigh"),Some(&json!({"high":99})))["maxThinkingTokens"],99.0);
    }
    #[test]
    fn legacy_budgets_reject_zero_and_no_reasoning_has_no_options() {assert!(thinking_options(&json!({"id":"older"}),None,None).is_empty());assert_eq!(thinking_options(&json!({"id":"older"}),Some("low"),Some(&json!({"low":0})))["maxThinkingTokens"],8192.0);}
    #[test]
    fn setting_sources_default_matrix_and_explicit_override() {
        use crate::auth_lane::TokenInjection::*;
        let settings=crate::settings::load(&json!({}),&Value::Null,&Default::default());for mode in ["full","override","preset-append"] {for lane in [Ambient,OAuthSlots,ConfigDir] {assert_eq!(setting_sources(&settings,mode,lane),if mode=="preset-append"&&lane==Ambient {json!(["user","project"])}else {json!([])});}}
        let settings=crate::settings::load(&json!({"anthropicSubscriptionProvider":{"settingSources":["local"]}}),&Value::Null,&Default::default());assert_eq!(setting_sources(&settings,"full",OAuthSlots),json!(["local"]));
    }
    #[test]
    fn tool_less_forces_strict_and_single_turn_without_exposing_tools() {
        let settings=crate::settings::load(&json!({"anthropicSubscriptionProvider":{"strictMcpConfig":false}}),&Value::Null,&Default::default());let options=tool_options(&settings,true,false,None);assert_eq!(options["tools"],json!([]));assert_eq!(options["maxTurns"],1);assert_eq!(options["extraArgs"],json!({"strict-mcp-config":null}));let options=tool_options(&settings,false,true,None);assert_eq!(options["tools"],json!([]));assert!(!options.contains_key("maxTurns"));assert!(!options.contains_key("extraArgs"));
    }
}
