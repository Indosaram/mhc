pub fn inject_mcp_instructions(instructions:&str,system_prompt:&str)->Option<String> {
    if instructions.is_empty() || system_prompt.contains(instructions){None}else{Some(format!("{system_prompt}\n\n{instructions}"))}
}
pub fn build_mcp_instructions_block<'a>(servers:impl IntoIterator<Item=(&'a str,&'a str)>)->String {
    servers.into_iter().filter(|(_,instructions)|!instructions.is_empty()).map(|(name,instructions)|{
        let capped=String::from_utf16_lossy(&instructions.encode_utf16().take(4000).collect::<Vec<_>>());
        format!("<mcp_instructions server=\"{}\">\n{}\n</mcp_instructions>",escape_xml(name),escape_xml(&capped))
    }).collect::<Vec<_>>().join("\n\n")
}
fn escape_xml(value:&str)->String {value.replace('&',"&amp;").replace('<',"&lt;").replace('>',"&gt;").replace('"',"&quot;").replace('\'',"&apos;")}
