pub fn inject_mcp_instructions(instructions:&str,system_prompt:&str)->Option<String> {
    if instructions.is_empty() || system_prompt.contains(instructions){None}else{Some(format!("{system_prompt}\n\n{instructions}"))}
}
pub fn build_mcp_instructions_block<'a>(servers:impl IntoIterator<Item=(&'a str,&'a str)>)->String {
    servers.into_iter().filter(|(_,instructions)|!instructions.is_empty()).map(|(name,instructions)|{
        let capped=String::from_utf16_lossy(&instructions.encode_utf16().take(4000).collect::<Vec<_>>());
        format!("<mcp_instructions server=\"{}\">\n{}\n</mcp_instructions>",escape_xml(name),escape_xml(&capped))
    }).collect::<Vec<_>>().join("\n\n")
}
pub async fn refresh_mcp_instructions_for_session(service:&crate::service::McpService)->String {
    let mut instructions=Vec::new();
    for (name,connection) in &service.connections {
        let entry=connection.entry.lock().await;
        let text=if entry.connection.state()==crate::connection::ServerConnectionState::Connected {
            match entry.connection.client(){Ok(client)=>client.instructions.read().await.clone(),Err(_)=>None}
        }else{entry.cached_catalog.as_ref().and_then(|catalog|catalog.instructions.clone())};
        if let Some(text)=text {instructions.push((name.clone(),text));}
    }
    build_mcp_instructions_block(instructions.iter().map(|(name,text)|(name.as_str(),text.as_str())))
}
fn escape_xml(value:&str)->String {value.replace('&',"&amp;").replace('<',"&lt;").replace('>',"&gt;").replace('"',"&quot;").replace('\'',"&apos;")}
