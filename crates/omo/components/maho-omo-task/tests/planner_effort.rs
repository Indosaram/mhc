use std::sync::Arc;
use maho_omo_task::planner::create_task_child_planner;
use senpi_task::{agents::map_omo_config_agents,host::{SenpiModelRegistry,HostError},manager::types::{ManagerStartSpec,ResolvedChildPlan}};
use serde_json::{Value,json};
struct Registry(Value);
impl SenpiModelRegistry for Registry { fn get_available(&self)->Result<Value,HostError> { Ok(json!([self.0])) } fn find(&self,provider:&str,id:&str)->Option<Value> { (self.0["provider"]==provider && self.0["id"]==id).then(|| self.0.clone()) } }
fn plan(agent:Value)->ResolvedChildPlan {
    let config=json!({"agents":{"explore":agent}}); let agents=map_omo_config_agents(&config).into_iter().collect();
    let registry=Arc::new(Registry(json!({"provider":"openai","id":"tuned"})));
    let planner=create_task_child_planner(config,agents,Arc::new(move || Some(registry.clone())));
    planner(&ManagerStartSpec { subagent_type:Some("explore".into()),prompt:"work".into(),parent_session_id:"parent".into(),depth:1,..Default::default() }).expect("resolved")
}
#[test] fn model_entry_effort_becomes_child_variant() { let result=plan(json!({"models":[{"model":"openai/tuned","reasoningEffort":"minimal"}]})); assert_eq!(result.variant.as_deref(),Some("minimal")); assert_eq!(result.resolved_model.expect("model").reasoning_effort.as_deref(),Some("minimal")); }
#[test] fn entry_effort_wins_over_entry_variant() { assert_eq!(plan(json!({"models":[{"model":"openai/tuned","variant":"high","reasoningEffort":"minimal"}]})).variant.as_deref(),Some("minimal")); }
#[test] fn entry_effort_wins_over_agent_variant() { assert_eq!(plan(json!({"variant":"high","models":[{"model":"openai/tuned","reasoningEffort":"minimal"}]})).variant.as_deref(),Some("minimal")); }
#[test] fn plain_model_does_not_invent_variant() { assert!(plan(json!({"models":["openai/tuned"]})).variant.is_none()); }
#[test] fn category_preserves_safe_registry_display_name() { let registry=Arc::new(Registry(json!({"provider":"openai","id":"tuned","name":"Friendly Model"}))); let planner=create_task_child_planner(json!({"categories":{"quick":{"model":"openai/tuned"}}}),vec![],Arc::new(move || Some(registry.clone()))); assert_eq!(planner(&ManagerStartSpec { category:Some("quick".into()),..Default::default() }).expect("resolved").resolved_model.expect("model").display,"Friendly Model"); }
#[test] fn category_rejects_registry_display_control_sequences() { let registry=Arc::new(Registry(json!({"provider":"openai","id":"tuned","name":"unsafe\u{1b}[31m"}))); let planner=create_task_child_planner(json!({"categories":{"quick":{"model":"openai/tuned"}}}),vec![],Arc::new(move || Some(registry.clone()))); assert_eq!(planner(&ManagerStartSpec { category:Some("quick".into()),..Default::default() }).expect("resolved").resolved_model.expect("model").display,"openai/tuned"); }
