use maho_ext_api::{ExtensionApi,ExtensionFailure,ToolDefinition};

pub fn register_tools_preserving_active_set(api:&mut ExtensionApi,mut tools:Vec<ToolDefinition>,intended_active_tools:Option<Vec<String>>)->Result<(),ExtensionFailure> {
    let intended=match intended_active_tools {Some(names)=>names,None=>api.get_active_tools()?};
    let collator=icu_collator::Collator::try_new(Default::default(),Default::default()).expect("compiled collation data is available");
    tools.sort_by(|left,right|collator.compare(&left.name,&right.name));
    for tool in tools {api.register_tool(tool);}
    api.set_active_tools(intended)
}
/// Registrar-backed form used by the MCP exposure path, where registration
/// happens at runtime through the retained `ExtensionApi` registrar rather than
/// during the factory. Sorting matches upstream `registerToolsPreservingActiveSet`
/// (`name.localeCompare`, i.e. locale order).
pub fn register_tools_preserving_active_set_with(registrar:&dyn crate::tool_registrar::McpToolRegistrar,mut tools:Vec<ToolDefinition>,intended_active_tools:Option<Vec<String>>)->Result<(),ExtensionFailure> {
    let intended=match intended_active_tools {Some(names)=>names,None=>registrar.get_active_tools()?};
    let collator=icu_collator::Collator::try_new(Default::default(),Default::default()).expect("compiled collation data is available");
    tools.sort_by(|left,right|collator.compare(&left.name,&right.name));
    for tool in tools {registrar.register_tool(tool)?;}
    registrar.set_active_tools(intended)
}
