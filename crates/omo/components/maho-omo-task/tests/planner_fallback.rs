use std::sync::Arc;
use maho_omo_task::planner::create_task_child_planner;
use senpi_task::{host::{SenpiModelRegistry,HostError},manager::types::{ManagerStartSpec,ResolvedChildPlan}};
use serde_json::{Value,json};
struct Registry(Vec<Value>);
impl SenpiModelRegistry for Registry {
    fn get_available(&self)->Result<Value,HostError> { Ok(json!(self.0)) }
    fn find(&self,provider:&str,id:&str)->Option<Value> { self.0.iter().find(|model| model["provider"]==provider&&model["id"]==id).cloned() }
}
fn plan(config:Value,models:Vec<Value>)->ResolvedChildPlan {
    let registry=Arc::new(Registry(models)); let planner=create_task_child_planner(config,vec![],Arc::new(move || Some(registry.clone())));
    planner(&ManagerStartSpec { category:Some("quick".into()),prompt:"work".into(),parent_session_id:"parent".into(),..Default::default() }).expect("plan")
}
#[test] fn configured_runtime_fallbacks_keep_requested_identity_and_ordered_effort() {
    let result=plan(json!({"categories":{"quick":{"model":"kimi-coding/kimi-for-coding-highspeed-unlocked","reasoningEffort":"minimal","fallback_models":[{"model":"openai-codex/gpt-5.6-luna-fast","reasoningEffort":"minimal"},{"model":"example-gateway/z-ai/glm-5.2-ultrafast-unlocked","reasoningEffort":"none"}]}}}),vec![json!({"provider":"kimi-coding","id":"kimi-for-coding-highspeed-unlocked"}),json!({"provider":"openai-codex","id":"gpt-5.6-luna-fast"}),json!({"provider":"example-gateway","id":"z-ai/glm-5.2-ultrafast-unlocked"})]);
    let requested=result.requested_model.expect("requested"); assert_eq!(requested.provider,"kimi-coding"); assert_eq!(requested.model_id,"kimi-for-coding-highspeed-unlocked"); let fallbacks=result.fallback_models.expect("fallbacks"); assert_eq!(fallbacks.len(),2); assert_eq!(fallbacks[0].provider,"openai-codex"); assert_eq!(fallbacks[0].reasoning_effort.as_deref(),Some("minimal")); assert_eq!(fallbacks[1].provider,"example-gateway"); assert_eq!(fallbacks[1].model_id,"z-ai/glm-5.2-ultrafast-unlocked"); assert_eq!(fallbacks[1].reasoning_effort.as_deref(),Some("none"));
}
#[test] fn unavailable_builtin_head_preserves_requested_head_and_remaining_rungs() {
    let result=plan(json!({}),vec![json!({"provider":"openai-codex","id":"gpt-5.6-luna-fast"}),json!({"provider":"opencode-go","id":"minimax-m3"})]); assert_eq!(result.model,"openai-codex/gpt-5.6-luna-fast"); assert_eq!(result.requested_model.expect("requested").provider,"kimi-coding"); assert_eq!(result.resolved_model.expect("resolved").variant.as_deref(),Some("low")); let fallbacks=result.fallback_models.expect("fallbacks"); assert_eq!(fallbacks.len(),1); assert_eq!(fallbacks[0].provider,"opencode-go"); assert_eq!(fallbacks[0].variant.as_deref(),Some("max"));
}
#[test] fn user_fallback_on_chain_rung_keeps_priority_without_duplicate_rung() {
    let result=plan(json!({"categories":{"quick":{"fallback_models":[{"model":"openai-codex/gpt-5.6-luna-fast","variant":"low"}]}}}),vec![json!({"provider":"openai-codex","id":"gpt-5.6-luna-fast"}),json!({"provider":"opencode-go","id":"minimax-m3"})]); assert_eq!(result.model,"openai-codex/gpt-5.6-luna-fast"); let fallbacks=result.fallback_models.expect("fallbacks"); assert_eq!(fallbacks.len(),1); assert_eq!(fallbacks[0].provider,"opencode-go"); assert_eq!(fallbacks[0].variant.as_deref(),Some("max"));
}
