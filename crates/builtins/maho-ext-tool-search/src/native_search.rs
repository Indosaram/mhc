use std::collections::BTreeSet;
use std::sync::Arc;
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
    transform_anthropic_native_tool_search(target,payload,config).unwrap_or_else(||payload.clone())
}
fn transform_anthropic_native_tool_search(target:Option<&AnthropicToolSearchTarget<'_>>,payload:&Value,config:&AnthropicNativeInjectionConfig<'_>)->Option<Value> {
    if !supports_anthropic_native_tool_search(target) || !payload.is_object() { return None; }
    let mut tools=payload.get("tools").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut names:BTreeSet<_>=tools.iter().filter_map(|t|t.get("name").and_then(Value::as_str).map(str::to_owned)).collect();
    for doc in config.catalog {
        if config.search_tool_name==Some(doc.name.as_str()) || names.contains(&doc.name) || !(config.is_deferrable)(&doc.name) { continue; }
        let Some(def)=(config.get_tool_definition)(&doc.name) else { continue; };
        let Some(parameters)=def.parameters else { continue; };
        tools.push(json!({"name":doc.name,"description":def.description.as_deref().or(doc.description.as_deref()).unwrap_or(&doc.label),"input_schema":parameters,"defer_loading":true}));
        names.insert(doc.name.clone());
    }
    if tools.len()>ANTHROPIC_MAX_TOOLS { return None; }
    for tool in &mut tools {
        let Some(name)=tool.get("name").and_then(Value::as_str) else { continue; };
        if config.search_tool_name==Some(name) || !(config.is_deferrable)(name) || tool.get("cache_control").is_some() || tool.get("defer_loading")==Some(&Value::Bool(true)) { continue; }
        if let Some(object)=tool.as_object_mut() { object.insert("defer_loading".into(),Value::Bool(true)); }
    }
    if !tools.iter().any(|t|t.get("type").and_then(Value::as_str)==Some(ANTHROPIC_TOOL_SEARCH_TYPE)) { tools.push(json!({"type":ANTHROPIC_TOOL_SEARCH_TYPE,"name":ANTHROPIC_TOOL_SEARCH_NAME})); }
    if tools.len()>ANTHROPIC_MAX_TOOLS { return None; }
    let mut result=payload.clone(); result["tools"]=Value::Array(tools); Some(result)
}
pub fn build_tool_reference_blocks(names:&[String])->Vec<Value> { names.iter().map(|name|json!({"type":"tool_reference","tool_name":name})).collect() }
/// Injection deps object (upstream `AnthropicNativeAdapterDeps`,
/// `tool-search/native-search.ts:114`): the resolved provider/config gate plus the
/// catalog accessors the adapter reads per request. It owns its data so the adapter
/// can hold it for the session; the borrowed `AnthropicNativeInjectionConfig` is
/// rebuilt from it on each call. The native port uses `get_catalog` (a getter, like
/// upstream) instead of the borrowed `catalog` slice the per-call config takes.
#[derive(Clone)]
pub struct AnthropicNativeAdapterDeps {
    pub search_tool_name:Option<String>,
    pub is_deferrable:Arc<dyn Fn(&str)->bool+Send+Sync>,
    pub get_catalog:Arc<dyn Fn()->Vec<ToolSearchDocument>+Send+Sync>,
    pub get_tool_definition:Arc<dyn Fn(&str)->Option<NativeToolDefinition>+Send+Sync>,
    /// Resolved provider/config gate (upstream `enabled()`).
    pub enabled:Arc<dyn Fn()->bool+Send+Sync>,
    /// Invoked once when a 400 forces the local-search fallback (upstream `onFallback?`).
    pub on_fallback:Option<Arc<dyn Fn(&str)+Send+Sync>>,
}
#[derive(Default)]
pub struct AnthropicNativeToolSearchAdapter { pub disabled:bool, injected_last_request:bool, pub fallback_reason:Option<String>, deps:Option<AnthropicNativeAdapterDeps> }
impl AnthropicNativeToolSearchAdapter {
    /// Upstream constructor (`new AnthropicNativeToolSearchAdapter(deps)`).
    pub fn new(deps:AnthropicNativeAdapterDeps)->Self {Self {disabled:false,injected_last_request:false,fallback_reason:None,deps:Some(deps)}}
    pub fn apply_before_request(&mut self,target:Option<&AnthropicToolSearchTarget<'_>>,payload:&Value,enabled:bool,config:&AnthropicNativeInjectionConfig<'_>)->Value {
        self.injected_last_request=false;
        if self.disabled || !enabled { return payload.clone(); }
        let next=transform_anthropic_native_tool_search(target,payload,config);
        self.injected_last_request=next.is_some(); next.unwrap_or_else(||payload.clone())
    }
    /// Deps-based injection (upstream `applyBeforeRequest(target, payload)`): reads
    /// the catalog and the provider/config gate from the bound deps instead of the
    /// per-call arguments. No-op when no deps were bound.
    pub fn apply_before_request_with_deps(&mut self,target:Option<&AnthropicToolSearchTarget<'_>>,payload:&Value)->Value {
        self.injected_last_request=false;
        let Some(deps)=self.deps.clone() else {return payload.clone();};
        if self.disabled || !(deps.enabled)() { return payload.clone(); }
        let catalog=(deps.get_catalog)();
        let config=AnthropicNativeInjectionConfig {search_tool_name:deps.search_tool_name.as_deref(),is_deferrable:deps.is_deferrable.as_ref(),catalog:&catalog,get_tool_definition:deps.get_tool_definition.as_ref()};
        let next=transform_anthropic_native_tool_search(target,payload,&config);
        self.injected_last_request=next.is_some(); next.unwrap_or_else(||payload.clone())
    }
    pub fn note_response_status(&mut self,status:u16)->Option<&str> {
        if status!=400 || !self.injected_last_request || self.disabled { return None; }
        self.disabled=true; self.fallback_reason=Some("Anthropic returned 400 for native tool-search; disabled it and fell back to local tool_search for this session.".into());
        if let Some(deps)=self.deps.as_ref() && let Some(on_fallback)=&deps.on_fallback { on_fallback(self.fallback_reason.as_deref().unwrap_or_default()); }
        self.fallback_reason.as_deref()
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
    fn mcp_doc()->ToolSearchDocument { ToolSearchDocument{name:"mcp_x_alpha".into(),label:"alpha".into(),aliases:vec!["alpha".into()],description:Some("alpha tool".into()),search_text:None,keywords:vec![],source:crate::engine::document::ToolSearchSource::Mcp,group:"x".into(),owner_label:"x".into(),registration_id:"mcp\u{0}x\u{0}alpha".into()} }
    #[test] fn deps_gate_and_catalog_drive_injection_and_fallback() {
        let catalog=vec![mcp_doc()];let gate=Arc::new(std::sync::atomic::AtomicBool::new(true));let enabled=gate.clone();
        let fallbacks=Arc::new(std::sync::Mutex::new(Vec::<String>::new()));let observed=fallbacks.clone();
        let deps=AnthropicNativeAdapterDeps {search_tool_name:Some("tool_search".into()),is_deferrable:Arc::new(|name|name.starts_with("mcp_")),get_catalog:Arc::new(move ||catalog.clone()),get_tool_definition:Arc::new(|name|Some(NativeToolDefinition {description:Some(format!("{name} def")),parameters:Some(json!({"type":"object"}))})),enabled:Arc::new(move ||enabled.load(std::sync::atomic::Ordering::SeqCst)),on_fallback:Some(Arc::new(move |reason|observed.lock().unwrap().push(reason.to_owned()))) };
        let mut adapter=AnthropicNativeToolSearchAdapter::new(deps);
        let out=adapter.apply_before_request_with_deps(Some(&AnthropicToolSearchTarget::Api("anthropic-messages")),&json!({"tools":[{"name":"tool_search"}]}));
        assert!(out["tools"].as_array().unwrap().iter().any(|tool|tool["name"]=="mcp_x_alpha"&&tool["defer_loading"]==true));
        gate.store(false,std::sync::atomic::Ordering::SeqCst);
        let unchanged=json!({"tools":[]});assert_eq!(adapter.apply_before_request_with_deps(Some(&AnthropicToolSearchTarget::Api("anthropic-messages")),&unchanged),unchanged);
        assert!(adapter.note_response_status(400).is_none());
    }
    #[test] fn deps_400_disables_and_notifies_fallback_once() {
        let catalog=vec![mcp_doc()];let fallbacks=Arc::new(std::sync::Mutex::new(Vec::<String>::new()));let observed=fallbacks.clone();
        let deps=AnthropicNativeAdapterDeps {search_tool_name:Some("tool_search".into()),is_deferrable:Arc::new(|_|true),get_catalog:Arc::new(move ||catalog.clone()),get_tool_definition:Arc::new(|_|Some(NativeToolDefinition {description:None,parameters:Some(json!({"type":"object"}))})),enabled:Arc::new(||true),on_fallback:Some(Arc::new(move |reason|observed.lock().unwrap().push(reason.to_owned()))) };
        let mut adapter=AnthropicNativeToolSearchAdapter::new(deps);
        adapter.apply_before_request_with_deps(Some(&AnthropicToolSearchTarget::Api("anthropic-messages")),&json!({"tools":[]}));
        assert!(adapter.note_response_status(400).is_some());assert!(adapter.disabled);
        assert_eq!(fallbacks.lock().unwrap().len(),1);
        assert!(adapter.note_response_status(400).is_none());assert_eq!(fallbacks.lock().unwrap().len(),1);
    }
    #[test] fn deps_gate_off_leaves_payload_untouched() {
        let deps=AnthropicNativeAdapterDeps {search_tool_name:None,is_deferrable:Arc::new(|_|true),get_catalog:Arc::new(Vec::new),get_tool_definition:Arc::new(|_|None),enabled:Arc::new(||false),on_fallback:None};
        let mut adapter=AnthropicNativeToolSearchAdapter::new(deps);
        let payload=json!({"tools":[{"name":"read"}]});
        assert_eq!(adapter.apply_before_request_with_deps(Some(&AnthropicToolSearchTarget::Api("anthropic-messages")),&payload),payload);
        assert!(adapter.note_response_status(400).is_none());
    }
}
