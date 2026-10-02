use serde_json::{Value,json};
use crate::custom_tools_schema::{ToolSchema,json_schema_to_shape};
pub const SERVER_NAME:&str="custom-tools";
pub struct CustomTool {pub name:String,pub description:String,pub input_schema:ToolSchema,pub wire_schema:Value}
pub struct CustomToolServer {pub name:&'static str,pub version:&'static str,pub tools:Vec<CustomTool>}
pub fn deny_custom_tool_execution()->Value {json!({"content":[{"type":"text","text":crate::tools::TOOL_EXECUTION_DENIED_MESSAGE}],"isError":true})}
pub fn build_custom_tool_server(tools:&[Value])->Option<CustomToolServer> {
    if tools.is_empty() {return None;}
    Some(CustomToolServer {name:SERVER_NAME,version:"1.0.0",tools:tools.iter().map(|tool|CustomTool {name:tool["name"].as_str().expect("tool name").into(),description:tool["description"].as_str().expect("tool description").into(),input_schema:ToolSchema::Object(json_schema_to_shape(&tool["parameters"])),wire_schema:tool["parameters"].clone()}).collect()})
}
impl CustomToolServer {
    pub fn rpc(&self,message:&Value)->Value {
        if message.get("id").is_none() {return json!({"jsonrpc":"2.0","result":{},"id":0});}
        let result=match message["method"].as_str() {
            Some("initialize")=>Ok(json!({"protocolVersion":message["params"]["protocolVersion"],"capabilities":{"tools":{}},"serverInfo":{"name":self.name,"version":self.version}})),
            Some("ping")=>Ok(json!({})),
            Some("tools/list")=>Ok(json!({"tools":self.tools.iter().map(|tool|json!({"name":tool.name,"description":tool.description,"inputSchema":tool.wire_schema})).collect::<Vec<_>>() })),
            Some("tools/call")=>self.call(message["params"]["name"].as_str().unwrap_or(""),message["params"].get("arguments").unwrap_or(&json!({}))),
            _=>return json!({"jsonrpc":"2.0","id":message["id"],"error":{"code":-32601,"message":"Method not found"}}),
        };
        match result {Ok(result)=>json!({"jsonrpc":"2.0","id":message["id"],"result":result}),Err(error)=>json!({"jsonrpc":"2.0","id":message["id"],"error":{"code":-32602,"message":error.to_string()}})}
    }
    pub fn call(&self,name:&str,arguments:&Value)->anyhow::Result<Value> {
        let tool=self.tools.iter().find(|tool|tool.name==name).ok_or_else(||anyhow::anyhow!("Unknown tool: {name}"))?;
        if !tool.input_schema.accepts(arguments) {anyhow::bail!("Invalid tool arguments: {name}");}Ok(deny_custom_tool_execution())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_server_validates_schema_but_never_executes_host_tool() {
        assert!(build_custom_tool_server(&[]).is_none());let server=build_custom_tool_server(&[json!({"name":"searchRepo","description":"Search files","parameters":{"type":"object","properties":{"query":{"type":"string"}},"required":["query"]}})]).expect("server");assert_eq!(server.name,SERVER_NAME);assert_eq!(server.version,"1.0.0");assert!(server.call("searchRepo",&json!({})).is_err());assert!(server.call("unknown",&json!({"query":"symbol"})).is_err());assert_eq!(server.call("searchRepo",&json!({"query":"symbol"})).expect("call")["isError"],true);
    }
}
