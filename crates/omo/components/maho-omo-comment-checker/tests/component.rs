mod support;
use maho_ext_api::*;
#[tokio::test] async fn failed_mutation_does_not_resolve_binary() {let mut api=ExtensionApi::new(LoadedExtension::new("checker","/tmp".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());maho_omo_comment_checker::component::CommentCheckerComponent{options:maho_omo_comment_checker::types::CommentCheckerComponentOptions{resolve_binary:Some(std::sync::Arc::new(||panic!("must not resolve"))),..Default::default()},..Default::default()}.register(&mut api);let mut event=ExtensionEvent::ToolResult(ToolResultEvent{tool_name:"write".into(),tool_call_id:"id".into(),input:serde_json::json!({"path":"file"}),content:vec![],details:None,is_error:true,usage:None});assert!(matches!(api.registered.handlers[&EventKind::ToolResult][0](&mut event,&support::context()).await.expect("dispatch"),EventResult::None));}
#[tokio::test] async fn feedback_once_per_normalized_path_and_reset_per_turn()->Result<(),Box<dyn std::error::Error>> {
 use std::os::unix::fs::PermissionsExt;
 let root=tempfile::tempdir()?;let binary=root.path().join("checker");std::fs::write(&binary,"#!/bin/sh\ncat >/dev/null\nprintf 'issue' >&2\nexit 2\n")?;std::fs::set_permissions(&binary,std::fs::Permissions::from_mode(0o700))?;
 let mut api=ExtensionApi::new(LoadedExtension::new("checker",root.path().into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());maho_omo_comment_checker::component::CommentCheckerComponent{package_binary:Some(binary),..Default::default()}.register(&mut api);
 let mut ctx=support::context();ctx.cwd=root.path().into();
 for (path,expected) in [("sub/../file.rs",true),("file.rs",false)] {
  let mut event=ExtensionEvent::ToolResult(ToolResultEvent{tool_name:"write".into(),tool_call_id:"id".into(),input:serde_json::json!({"path":path,"content":"// comment"}),content:vec![ToolContent::text("original")],details:None,is_error:false,usage:None});
  let result=api.registered.handlers[&EventKind::ToolResult][0](&mut event,&ctx).await?;assert_eq!(matches!(result,EventResult::ToolResult(_)),expected);
 }
 let mut event=ExtensionEvent::TurnStart{turn_index:1,timestamp:0};api.registered.handlers[&EventKind::TurnStart][0](&mut event,&ctx).await?;
 let mut event=ExtensionEvent::ToolResult(ToolResultEvent{tool_name:"write".into(),tool_call_id:"id".into(),input:serde_json::json!({"path":"file.rs","content":"// comment"}),content:vec![],details:None,is_error:false,usage:None});assert!(matches!(api.registered.handlers[&EventKind::ToolResult][0](&mut event,&ctx).await?,EventResult::ToolResult(_)));Ok(())
}
