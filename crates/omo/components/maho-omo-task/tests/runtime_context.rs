pub mod support;
use std::{path::Path,sync::Arc};
use maho_ext_api::*;
use maho_omo_task::runtime_context::TaskRuntimeContext;
use senpi_task::completion::{ParentState,TransitionReason};
struct Session;
impl ToolSessionManager for Session { fn session_id(&self)->&str { "session-a" } fn session_file(&self)->Option<&Path> { Some(Path::new("/tmp/sessions/session-a.jsonl")) } }
impl SessionManager for Session { fn get_entries(&self)->Vec<SessionEntry> { vec![] } fn get_branch(&self)->Vec<SessionEntry> { vec![] } fn get_leaf_id(&self)->Option<String> { None } fn get_session_name(&self)->Option<String> { None } }
#[test] fn capture_retains_exact_session_file() { let mut context=support::context(); context.session_manager=Arc::new(Session); let mut runtime=TaskRuntimeContext::new("/project".into()); runtime.capture_from(&context); assert_eq!(runtime.session_id(),Some("session-a")); assert_eq!(runtime.session_file(),Some(Path::new("/tmp/sessions/session-a.jsonl"))); }
#[test] fn capture_uses_session_cwd_and_exact_registry() { let context=support::context(); let mut runtime=TaskRuntimeContext::new("/project".into()); runtime.capture_from(&context); assert_eq!(runtime.cwd(),Path::new("/tmp")); assert!(Arc::ptr_eq(runtime.model_registry().expect("registry"),&context.model_registry)); }
#[test] fn transition_overrides_live_idle_facts_until_released() { let mut context=support::context(); context.is_idle_fn=Arc::new(|| false); let mut runtime=TaskRuntimeContext::new("/project".into()); runtime.set_transition(Some(TransitionReason::Compacting)); runtime.capture_from(&context); assert_eq!(runtime.parent_state(),ParentState::Compacting); runtime.set_transition(None); assert_eq!(runtime.parent_state(),ParentState::Streaming); runtime.clear_ui(); assert!(runtime.ui().is_none()); assert_eq!(runtime.session_id(),Some("session")); }
