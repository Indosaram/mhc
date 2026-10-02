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
            "status"|"list"=>Ok(service.status("MCP servers").await),
            "test"=>service.test_server(name).await.map(|(elapsed,count)|format!("MCP test {name} ok ({elapsed:.0}ms): {count} tools")).map_err(|error|error.to_string()),
            "reconnect"=>service.reconnect_server(name).await.map(|()|format!("MCP reconnect {name} connected")).map_err(|error|error.to_string()),
            "auth"|"auth-start"|"auth-complete"|"logout"=>crate::auth::commands_auth_dispatch::handle_mcp_auth_command(subcommand,&args[1..],ctx.has_ui,ctx.ui.clone(),&mut service).await,
            "logs"=>{if let Some(connection)=service.connections.get(name){let entry=connection.entry.lock().await;let lines=entry.logger.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get_ring_buffer();let lines=lines.into_iter().rev().take(20).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>();Ok(if lines.is_empty(){format!("MCP logs for {name}: (empty)")}else{lines.join("\n")})}else{Err(format!("Unknown MCP server: {name}"))}},
            "add"=>{
                if name.is_empty() || args.len()<3 {Err("Usage: /mcp add <name> <command...|url>".into())}
                else if !ctx.has_ui {Err("Cannot add MCP server without UI confirmation.".into())}
                else if !ctx.ui.confirm("Add MCP server?",&format!("{name}: {}",args[2..].join(" ")),Default::default()).await {Ok("MCP add cancelled".into())}
                else {let config=crate::commands::parse_server_config(&args[2..]);match crate::config_edit::add_global_mcp_server(&ctx.agent_dir,name,&config) {Err(error)=>Err(error.to_string()),Ok(_)=>{let env=std::env::vars().collect();service.attach_session(&ctx.cwd,&ctx.agent_dir,&env,ctx.is_project_trusted(),&ctx.registered_mcp_servers).await.map(|()|format!("Added MCP server {name}")).map_err(|error|error.to_string())}}}
            },
            "enable"|"disable"=>{
                match crate::config_edit::set_global_mcp_server_enabled(&ctx.agent_dir,name,subcommand=="enable") {Err(error)=>Err(error.to_string()),Ok(false)=>Err(format!("MCP server {name} is not in the global config file")),Ok(true)=>{let env=std::env::vars().collect();service.attach_session(&ctx.cwd,&ctx.agent_dir,&env,ctx.is_project_trusted(),&ctx.registered_mcp_servers).await.map(|()|format!("{} MCP server {name}",if subcommand=="enable"{"Enabled"}else{"Disabled"})).map_err(|error|error.to_string())}}
            },
            _=>Err(format!("Unknown MCP subcommand: {subcommand}")),
        };
        match result {Ok(message)=>ctx.ui.notify(&message,maho_ext_api::NotificationType::Info),Err(error)=>ctx.ui.notify(&error,maho_ext_api::NotificationType::Error)}Ok(())
    })}));
    let start=service.clone();api.on(EventKind::SessionStart,Arc::new(move |_,ctx|{let service=start.clone();Box::pin(async move {
        let env=std::env::vars().collect();
        let mut service=service.lock().await;service.set_elicitation_ui(if ctx.has_ui{Some(ctx.ui.clone())}else{None});
        service.attach_session(&ctx.cwd,&ctx.agent_dir,&env,ctx.is_project_trusted(),&ctx.registered_mcp_servers).await.map_err(|error|ExtensionFailure::new(error.to_string()))?;
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
