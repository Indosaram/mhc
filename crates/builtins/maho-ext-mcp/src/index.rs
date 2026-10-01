use std::sync::Arc;
use maho_ext_api::{ExtensionApi,EventKind,EventResult,ExtensionEvent,ExtensionFailure,BeforeAgentStartEventResult};
use crate::{service::McpService,host_registry::HostMcpRegistry};
pub fn register_mcp_lifecycle(api:&mut ExtensionApi,registry:Arc<HostMcpRegistry>,owner:u64)->Arc<tokio::sync::Mutex<McpService>> {
    let service=Arc::new(tokio::sync::Mutex::new(McpService::new(registry,owner)));
    let command_service=service.clone();
    api.register_command("mcp",Some("Manage MCP servers".into()),Some("status | test | reconnect | auth-start | auth-complete | logout".into()),Arc::new(move |raw,ctx|{let service=command_service.clone();Box::pin(async move {
        let args=crate::commands::split_command_args(raw).map_err(|error|ExtensionFailure::new(error.to_string()))?;
        let subcommand=args.first().map_or("status",String::as_str);let name=args.get(1).map_or("",String::as_str);let mut service=service.lock().await;
        let result:Result<String,String>=match subcommand {
            "status"|"list"=>{let snapshots=service.server_snapshots().await;Ok(snapshots.iter().map(|snapshot|format!("{}: {:?}",snapshot.name,snapshot.lifecycle_state)).collect::<Vec<_>>().join("\n"))},
            "test"=>service.connect_server(name).await.map(|()|format!("MCP test {name} ok")).map_err(|error|error.to_string()),
            "reconnect"=>service.reconnect_server(name).await.map(|()|format!("MCP reconnect {name} connected")).map_err(|error|error.to_string()),
            "auth-start"=>service.auth_start(name).await.map(|url|format!("Open this URL, approve, then run /mcp auth-complete {name} <redirect-url>:\n{url}")).map_err(|error|error.to_string()),
            "auth-complete"=>service.auth_complete(name,args.get(2).map_or("",String::as_str)).await.map(|()|format!("MCP server {name} authorized")).map_err(|error|error.to_string()),
            "logout"=>service.logout(name).await.map(|()|format!("MCP server {name} logged out")).map_err(|error|error.to_string()),
            _=>Err(format!("Unknown MCP subcommand: {subcommand}")),
        };
        match result {Ok(message)=>ctx.ui.notify(&message,maho_ext_api::NotificationType::Info),Err(error)=>ctx.ui.notify(&error,maho_ext_api::NotificationType::Error)}Ok(())
    })}));
    let start=service.clone();api.on(EventKind::SessionStart,Arc::new(move |_,ctx|{let service=start.clone();Box::pin(async move {
        let env=std::env::vars().collect();
        service.lock().await.attach_session(&ctx.cwd,&ctx.agent_dir,&env,ctx.is_project_trusted(),&ctx.registered_mcp_servers).await.map_err(|error|ExtensionFailure::new(error.to_string()))?;
        Ok(EventResult::None)
    })}));
    let shutdown=service.clone();api.on(EventKind::SessionShutdown,Arc::new(move |_,_|{let service=shutdown.clone();Box::pin(async move {service.lock().await.dispose().await.map_err(|error|ExtensionFailure::new(error.to_string()))?;Ok(EventResult::None)})}));
    let before=service.clone();api.on(EventKind::BeforeAgentStart,Arc::new(move |event,_|{let service=before.clone();Box::pin(async move {
        let ExtensionEvent::BeforeAgentStart(event)=event else{return Ok(EventResult::None);};let service=service.lock().await;
        service.wait_for_deferred_attach(std::time::Duration::from_millis(crate::startup_race::MCP_ATTACH_SETTLE_TIMEOUT_MS)).await;
        let mut instructions=Vec::new();
        for (name,connection) in &service.connections {let entry=connection.entry.lock().await;if let Some(cached)=&entry.cached_catalog && let Some(text)=&cached.instructions {instructions.push((name.clone(),text.clone()));}}
        let block=crate::instructions::build_mcp_instructions_block(instructions.iter().map(|(name,text)|(name.as_str(),text.as_str())));
        Ok(EventResult::BeforeAgentStart(BeforeAgentStartEventResult {message:None,system_prompt:crate::instructions::inject_mcp_instructions(&block,&event.system_prompt)}))
    })}));
    service
}
