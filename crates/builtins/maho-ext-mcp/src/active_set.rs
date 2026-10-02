use maho_ext_api::{ExtensionApi,ExtensionFailure,ToolDefinition};

pub fn register_tools_preserving_active_set(api:&mut ExtensionApi,mut tools:Vec<ToolDefinition>,intended_active_tools:Option<Vec<String>>)->Result<(),ExtensionFailure> {
    let intended=match intended_active_tools {Some(names)=>names,None=>api.get_active_tools()?};
    tools.sort_by(|left,right|left.name.cmp(&right.name));
    for tool in tools {api.register_tool(tool);}
    api.set_active_tools(intended)
}
