use maho_ext_cache_keepalive::*;
use maho_ai::types::{Model,Usage};
fn model()->Model{Model{id:"m".into(),name:"m".into(),api:"anthropic-messages".into(),provider:"anthropic".into(),base_url:"https://api.anthropic.com/v1".into(),reasoning:false,thinking_level_map:None,input:vec![],cost:Default::default(),context_window:100,max_tokens:10,sampling_params:None,headers:None,cache_retention:None,upstream_model_id:None,service_tier:None,recover_text_tool_calls:None,compat:None}}
#[test]
fn costs_use_prompt_tokens_and_each_cache_rate(){let mut model=model();model.cost.input=3.0;model.cost.cache_read=0.3;model.cost.cache_write=3.75;let usage=Usage{input:100,cache_read:200,cache_write:300,..Usage::default()};assert_eq!(projected_ping_cost(&model,Some(&usage)),0.00225);assert_eq!(actual_ping_cost(&model,&usage),0.001485);assert_eq!(projected_ping_cost(&model,None),0.0);}
#[test]
fn wait_margin_and_elapsed_time(){assert_eq!(next_delay_ms(1000.0,300.0,30.0,2000.0),269000.0);assert_eq!(next_delay_ms(1000.0,10.0,30.0,2000.0),0.0);assert_eq!(next_delay_ms(1000.0,10.0,-1.0,2000.0),9000.0);}
#[test]
fn only_native_anthropic_is_supported(){let mut model=model();assert!(is_warm_supported_model(&model));model.base_url="https://openrouter.ai/api/v1".into();assert!(!is_warm_supported_model(&model));}

#[test]
fn history_uses_last_assistant_not_trailing_user_or_tool() {
    use maho_agent::types::AgentMessage;
    use maho_ai::types::StopReason;
    use serde_json::json;
    let assistant = |timestamp, input, reason| serde_json::from_value::<AgentMessage>(json!({"role":"assistant","content":[],"api":"anthropic-messages","provider":"anthropic","model":"m","usage":{"input":input,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":input,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":reason,"timestamp":timestamp})).expect("assistant");
    let user = serde_json::from_value::<AgentMessage>(json!({"role":"user","content":"next","timestamp":999})).expect("user");
    let messages = vec![assistant(100,10,"stop"),assistant(200,20,"error"),user];
    let (usage,reason) = last_assistant_usage(&messages).expect("usage");
    assert_eq!(usage.input,20);
    assert_eq!(reason,StopReason::Error);
    assert_eq!(last_assistant_timestamp(&messages),Some(200));
    assert!(last_assistant_usage(&[]).is_none());
    assert!(last_assistant_timestamp(&[]).is_none());
    assert!(last_assistant_usage(&messages[2..]).is_none());
    assert!(last_assistant_timestamp(&messages[2..]).is_none());
}
