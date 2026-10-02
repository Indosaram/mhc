use std::collections::BTreeSet;
use serde_json::{Value,json};
use crate::{engine::document::ToolSearchDocument,native_support::{AnthropicToolSearchTarget,supports_anthropic_native_tool_search}};
pub const ANTHROPIC_TOOL_SEARCH_TYPE:&str="tool_search_tool_bm25_20251119";
pub const ANTHROPIC_TOOL_SEARCH_NAME:&str="tool_search_tool_bm25";
pub const ANTHROPIC_MAX_TOOLS:usize=10000;
pub struct NativeToolDefinition { pub description:Option<String>,pub parameters:Option<Value> }
pub struct AnthropicNativeInjectionConfig<'a> {
    pub search_tool_name:Option<&'a str>,
    pub is_deferrable:&'a dyn Fn(&str)->bool,
    pub catalog:&'a [ToolSearchDocument],
    pub get_tool_definition:&'a dyn Fn(&str)->Option<NativeToolDefinition>,
}
pub fn add_anthropic_native_tool_search(target:Option<&AnthropicToolSearchTarget<'_>>,payload:&Value,config:&AnthropicNativeInjectionConfig<'_>)->Value {
    if !supports_anthropic_native_tool_search(target) || !payload.is_object() { return payload.clone(); }
    let mut tools=payload.get("tools").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut names:BTreeSet<_>=tools.iter().filter_map(|t|t.get("name").and_then(Value::as_str).map(str::to_owned)).collect();
    for doc in config.catalog {
        if config.search_tool_name==Some(doc.name.as_str()) || names.contains(&doc.name) || !(config.is_deferrable)(&doc.name) { continue; }
        let Some(def)=(config.get_tool_definition)(&doc.name) else { continue; };
        let Some(parameters)=def.parameters else { continue; };
        tools.push(json!({"name":doc.name,"description":def.description.as_deref().or(doc.description.as_deref()).unwrap_or(&doc.label),"input_schema":parameters,"defer_loading":true}));
        names.insert(doc.name.clone());
    }
    if tools.len()>ANTHROPIC_MAX_TOOLS { return payload.clone(); }
    for tool in &mut tools {
        let Some(name)=tool.get("name").and_then(Value::as_str) else { continue; };
        if config.search_tool_name==Some(name) || !(config.is_deferrable)(name) || tool.get("cache_control").is_some() || tool.get("defer_loading")==Some(&Value::Bool(true)) { continue; }
        if let Some(object)=tool.as_object_mut() { object.insert("defer_loading".into(),Value::Bool(true)); }
    }
    if !tools.iter().any(|t|t.get("type").and_then(Value::as_str)==Some(ANTHROPIC_TOOL_SEARCH_TYPE)) { tools.push(json!({"type":ANTHROPIC_TOOL_SEARCH_TYPE,"name":ANTHROPIC_TOOL_SEARCH_NAME})); }
    if tools.len()>ANTHROPIC_MAX_TOOLS { return payload.clone(); }
    let mut result=payload.clone(); result["tools"]=Value::Array(tools); result
}
pub fn build_tool_reference_blocks(names:&[String])->Vec<Value> { names.iter().map(|name|json!({"type":"tool_reference","tool_name":name})).collect() }
#[derive(Default)]
pub struct AnthropicNativeToolSearchAdapter { pub disabled:bool, injected_last_request:bool, pub fallback_reason:Option<String> }
impl AnthropicNativeToolSearchAdapter {
    pub fn apply_before_request(&mut self,target:Option<&AnthropicToolSearchTarget<'_>>,payload:&Value,enabled:bool,config:&AnthropicNativeInjectionConfig<'_>)->Value {
        self.injected_last_request=false;
        if self.disabled || !enabled { return payload.clone(); }
        let next=add_anthropic_native_tool_search(target,payload,config);
        self.injected_last_request=supports_anthropic_native_tool_search(target) && payload.is_object() && next.get("tools").and_then(Value::as_array).is_some_and(|t|t.len()<=ANTHROPIC_MAX_TOOLS) && (next!=*payload || next.get("tools").and_then(Value::as_array).is_some_and(|t|t.iter().any(|t|t.get("type").and_then(Value::as_str)==Some(ANTHROPIC_TOOL_SEARCH_TYPE)))); next
    }
    pub fn note_response_status(&mut self,status:u16)->Option<&str> {
        if status!=400 || !self.injected_last_request || self.disabled { return None; }
        self.disabled=true; self.fallback_reason=Some("Anthropic returned 400 for native tool-search; disabled it and fell back to local tool_search for this session.".into()); self.fallback_reason.as_deref()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn upstream_payload()->Value {json!({"tools":[{"name":"tool_search","description":"search","input_schema":{}},{"name":"mcp_docs_get-library-docs","description":"docs","input_schema":{}}]})}
    fn upstream_injection(target:AnthropicToolSearchTarget<'_>)->Value {
        add_anthropic_native_tool_search(Some(&target),&upstream_payload(),&AnthropicNativeInjectionConfig{search_tool_name:Some("tool_search"),is_deferrable:&|name|name.starts_with("mcp_"),catalog:&[],get_tool_definition:&|_|None})
    }
    #[test] fn upstream_canonical_server_name() {
        let out=upstream_injection(AnthropicToolSearchTarget::Api("anthropic-messages"));
        let tools=out["tools"].as_array().unwrap();
        assert_eq!(tools.iter().filter(|tool|tool["type"]=="tool_search_tool_bm25_20251119").collect::<Vec<_>>(),vec![&json!({"type":"tool_search_tool_bm25_20251119","name":"tool_search_tool_bm25"})]);
    }
    #[test] fn upstream_local_search_is_resident() {
        assert_eq!(upstream_injection(AnthropicToolSearchTarget::Api("anthropic-messages"))["tools"][0],upstream_payload()["tools"][0]);
    }
    #[test] fn upstream_non_anthropic_payload_is_unchanged() {
        assert_eq!(upstream_injection(AnthropicToolSearchTarget::Api("openai-responses")),upstream_payload());
    }
    #[test] fn upstream_tool_reference_target_field() {
        assert_eq!(build_tool_reference_blocks(&["mcp_docs_get-library-docs".into(),"mcp_docs_resolve-library-id".into()]),vec![json!({"type":"tool_reference","tool_name":"mcp_docs_get-library-docs"}),json!({"type":"tool_reference","tool_name":"mcp_docs_resolve-library-id"})]);
    }
    #[test] fn upstream_proxy_payload_is_unchanged() {
        assert_eq!(upstream_injection(AnthropicToolSearchTarget::Model{api:"anthropic-messages",id:"claude-opus-5",provider:"openmodel",supports_tool_references:None}),upstream_payload());
    }
    #[test] fn upstream_model_server_name() {
        let out=upstream_injection(AnthropicToolSearchTarget::Model{api:"anthropic-messages",id:"claude-opus-5",provider:"anthropic",supports_tool_references:None});
        assert_eq!(out["tools"][2],json!({"type":"tool_search_tool_bm25_20251119","name":"tool_search_tool_bm25"}));
    }
    #[test] fn defer_preserves_cache_control_and_search() {
        let config=AnthropicNativeInjectionConfig{search_tool_name:Some("tool_search"),is_deferrable:&|_|true,catalog:&[],get_tool_definition:&|_|None};
        let out=add_anthropic_native_tool_search(Some(&AnthropicToolSearchTarget::Api("anthropic-messages")),&json!({"tools":[{"name":"tool_search"},{"name":"read","cache_control":{}},{"name":"write"}]}),&config);
        assert!(out["tools"][0].get("defer_loading").is_none()); assert!(out["tools"][1].get("defer_loading").is_none()); assert_eq!(out["tools"][2]["defer_loading"],true); assert_eq!(out["tools"][3]["type"],ANTHROPIC_TOOL_SEARCH_TYPE);
    }
    #[test] fn cap_is_noop() { let payload=json!({"tools":vec![json!({"name":"read"});10000]}); let config=AnthropicNativeInjectionConfig{search_tool_name:Some("tool_search"),is_deferrable:&|_|true,catalog:&[],get_tool_definition:&|_|None}; assert_eq!(add_anthropic_native_tool_search(Some(&AnthropicToolSearchTarget::Api("anthropic-messages")),&payload,&config),payload); }
    #[test] fn permanent_400_fallback() { let config=AnthropicNativeInjectionConfig{search_tool_name:None,is_deferrable:&|_|false,catalog:&[],get_tool_definition:&|_|None}; let mut adapter=AnthropicNativeToolSearchAdapter::default(); assert!(adapter.note_response_status(400).is_none()); adapter.apply_before_request(Some(&AnthropicToolSearchTarget::Api("anthropic-messages")),&json!({}),true,&config); assert!(adapter.note_response_status(400).is_some()); assert!(adapter.disabled); assert_eq!(adapter.apply_before_request(Some(&AnthropicToolSearchTarget::Api("anthropic-messages")),&json!({}),true,&config),json!({})); }
    #[test] fn references_use_tool_name() { assert_eq!(build_tool_reference_blocks(&["read".into()]),vec![json!({"type":"tool_reference","tool_name":"read"})]); }
}
