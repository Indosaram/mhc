use std::sync::{Arc,Mutex};
use maho_omo_task::engine_runners::{InProcessRunnerBuildContext,build_in_process_runner};
use senpi_task::{host::HostError,manager::types::{ManagedStartSpec,ManagedRunnerError},runners::{RunnerFailureKind,in_process::shared_tool_filter::ChildTool}};
struct Tool(&'static str); impl ChildTool for Tool { fn name(&self)->&str { self.0 } fn description(&self)->&str { "fixture" } fn execute(&self,_:&str,_:&serde_json::Value)->Result<serde_json::Value,HostError> { panic!("not executed") } }
#[test]
fn factory_filters_memory_and_task_family_tools_before_child_creation() {
    let root=tempfile::tempdir().expect("root"); let captured=Arc::new(Mutex::new(Vec::new())); let tools=captured.clone();
    let runner=build_in_process_runner(InProcessRunnerBuildContext { shared_parent_tools:["read","memory","memory_apply_patch","task_send","dag"].into_iter().map(|name| Arc::new(Tool(name)) as senpi_task::runners::in_process::shared_tool_filter::ChildToolRef).collect(),max_depth:2,parent_registry:Arc::new(|| None),create_session:Arc::new(move |options| { *tools.lock().expect("tools")=options.custom_tools.iter().map(|tool| tool.name().to_owned()).collect(); Err(HostError { message:"fixture stop".into() }) }) });
    let spec=ManagedStartSpec { task_id:"st_test".into(),cwd:root.path().to_string_lossy().into_owned(),state_dir:root.path().to_string_lossy().into_owned(),prompt:"work".into(),depth:1,..Default::default() };
    assert!(runner.start(&spec).is_err()); assert_eq!(*captured.lock().expect("captured"),["read"]);
}
#[test]
fn factory_adds_one_to_parent_depth_limit() {
    let runner=build_in_process_runner(InProcessRunnerBuildContext { shared_parent_tools:vec![],max_depth:2,parent_registry:Arc::new(|| None),create_session:Arc::new(|_| panic!("depth rejected before create")) });
    let result=runner.start(&ManagedStartSpec { task_id:"st_test".into(),depth:4,..Default::default() });
    assert!(matches!(result,Err(ManagedRunnerError::Runner(failure)) if failure.kind==RunnerFailureKind::DepthExceeded));
}
#[test] fn builtin_agents_exist_without_overrides() { let agents=maho_omo_task::engine_runners::resolve_task_agents(&serde_json::json!({})); assert_eq!(agents.keys().map(String::as_str).collect::<Vec<_>>(),["explore","librarian","metis","momus"]); assert_eq!(agents["explore"].execution_mode.as_deref(),Some("in-process")); }
#[test] fn model_overlay_preserves_builtin_prompt_and_tools() { let baseline=maho_omo_task::engine_runners::resolve_task_agents(&serde_json::json!({})); let agents=maho_omo_task::engine_runners::resolve_task_agents(&serde_json::json!({"agents":{"explore":{"model":"acme/custom-1"}}})); assert_eq!(agents["explore"].model.as_deref(),Some("acme/custom-1")); assert_eq!(agents["explore"].prompt,baseline["explore"].prompt); assert_eq!(agents["explore"].tools,baseline["explore"].tools); }
#[test] fn curated_execution_is_pinned_but_user_agent_remains_configurable() { let agents=maho_omo_task::engine_runners::resolve_task_agents(&serde_json::json!({"agents":{"explore":{"execution_mode":"process"},"scout":{"execution_mode":"process"}}})); assert_eq!(agents["explore"].execution_mode.as_deref(),Some("in-process")); assert_eq!(agents["scout"].execution_mode.as_deref(),Some("process")); }
#[test] fn user_agent_is_appended_and_disabled_builtin_retains_disable_flag() {
    let agents=maho_omo_task::engine_runners::resolve_task_agents(&serde_json::json!({"agents":{"momus":{"disable":true},"scout":{"description":"Project scout","prompt":"Scout the repo."}}})); assert_eq!(agents.keys().map(String::as_str).collect::<Vec<_>>(),["explore","librarian","metis","momus","scout"]); assert_eq!(agents["momus"].disable,Some(true)); assert_eq!(agents["momus"].execution_mode.as_deref(),Some("in-process")); assert_eq!(agents["scout"].prompt.as_deref(),Some("Scout the repo.")); assert_eq!(agents["scout"].description.as_deref(),Some("Project scout"));
}
