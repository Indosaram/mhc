use std::{collections::BTreeMap, path::Path, sync::Arc};
use maho_ext_api::{ToolExposure, ToolInfo};
use serde_json::Value;
use crate::engine::{bm25::{Bm25Result, Bm25SearchOptions, DEFAULT_BM25_PRECISION, build_bm25_index, normalize_tool_name, tokenize_tool_text}, document::{ToolSearchDocument, ToolSearchSource}, marker::{RehydratableToolSearchDocument, derive_extension_registration_id, rehydrate}};

pub trait ToolSearchRuntime: Send + Sync {
    fn get_all_tools(&self) -> Vec<ToolInfo>;
    fn get_active_tools(&self) -> Vec<String>;
    fn set_active_tools(&self, names: &[String]);
}
pub type ActivationHook = Arc<dyn Fn(&[String]) + Send + Sync>;
pub type RemovedToolHints = Arc<dyn Fn() -> BTreeMap<String, String> + Send + Sync>;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HiddenToolHint { pub name: String, pub hint: String }
pub struct ToolSearchService {
    runtime: Arc<dyn ToolSearchRuntime>,
    mcp_docs: Vec<ToolSearchDocument>,
    mcp_hook: Option<ActivationHook>,
    extension_docs: Vec<ToolSearchDocument>,
    extension_fingerprint: String,
    registry_generation: u64,
    history_scanned_generation: Option<u64>,
    registrar: Option<Arc<dyn Fn() + Send + Sync>>,
    removed_tool_hints: RemovedToolHints,
    native_injection_failure: Option<String>,
}
impl ToolSearchService {
    pub fn new(runtime: Arc<dyn ToolSearchRuntime>) -> Self {
        Self { runtime, mcp_docs:Vec::new(), mcp_hook:None, extension_docs:Vec::new(), extension_fingerprint:String::new(), registry_generation:0, history_scanned_generation:None, registrar:None, removed_tool_hints:Arc::new(BTreeMap::new), native_injection_failure:None }
    }
    pub fn bind_runtime(&mut self, runtime: Arc<dyn ToolSearchRuntime>) { self.runtime=runtime; }
    pub fn bind_tool_registrar(&mut self, registrar: Arc<dyn Fn() + Send + Sync>) { self.registrar=Some(registrar); }
    pub fn bind_removed_tool_hints(&mut self, provider: RemovedToolHints) { self.removed_tool_hints=provider; }
    pub fn note_native_injection_failure(&mut self, reason: String) { self.native_injection_failure=Some(reason); }
    pub fn take_native_injection_failure(&mut self) -> Option<String> { self.native_injection_failure.take() }
    pub fn hidden_tool_hints(&self, query: &str) -> Vec<HiddenToolHint> {
        let hints=(self.removed_tool_hints)();
        let by_name:BTreeMap<_,_>=hints.into_iter().map(|(name,hint)|(normalize_tool_name(&name),HiddenToolHint{name,hint})).collect();
        let mut matched=Vec::new();
        for candidate in std::iter::once(normalize_tool_name(query)).chain(tokenize_tool_text(query)) {
            if let Some(entry)=by_name.get(&candidate) && !matched.iter().any(|e:&HiddenToolHint|e.name==entry.name) { matched.push(entry.clone()); }
        }
        matched
    }
    pub fn get_tool_parameters(&self, name: &str) -> Option<Value> { self.runtime.get_all_tools().into_iter().find(|t|t.name==name).map(|t|t.parameters) }
    pub fn begin_session(&mut self) { self.mcp_docs.clear(); self.mcp_hook=None; self.registry_generation=self.registry_generation.wrapping_add(1); self.history_scanned_generation=None; self.refresh_extension_docs(); self.sync_lifecycle(); }
    pub fn feed(&mut self, docs: &[ToolSearchDocument], hook: ActivationHook) {
        self.mcp_docs=docs.iter().filter(|d|d.source==ToolSearchSource::Mcp && !d.name.is_empty() && !d.registration_id.is_empty()).cloned().collect();
        self.mcp_hook=Some(hook); self.registry_generation=self.registry_generation.wrapping_add(1); self.sync_lifecycle();
    }
    pub fn get_catalog(&mut self) -> Vec<ToolSearchDocument> { self.refresh_extension_docs(); self.mcp_docs.iter().chain(&self.extension_docs).cloned().collect() }
    pub fn search(&mut self, query: &str, limit: usize, options: &Bm25SearchOptions) -> Vec<Bm25Result> {
        let mut options=options.clone(); if options.precision.is_none() { options.precision=Some(DEFAULT_BM25_PRECISION); }
        build_bm25_index(&self.get_catalog()).search(query,limit,&options)
    }
    pub fn activate(&self, matches: &[Bm25Result]) -> Vec<String> { self.activate_names(&matches.iter().map(|m|m.name.clone()).collect::<Vec<_>>(),&matches.iter().map(|m|m.doc.clone()).collect::<Vec<_>>()); let active=self.runtime.get_active_tools(); matches.iter().filter(|m|active.contains(&m.name)).map(|m|m.name.clone()).collect() }
    pub fn activate_tool(&mut self, name: &str) -> bool { let catalog=self.get_catalog(); if !catalog.iter().any(|d|d.name==name) { return false; } self.activate_names(&[name.into()],&catalog); self.runtime.get_active_tools().iter().any(|n|n==name) }
    pub fn maybe_rehydrate_from_history(&mut self, messages: &[Value]) -> Vec<String> {
        let catalog=self.get_catalog(); if self.history_scanned_generation==Some(self.registry_generation) { return Vec::new(); }
        self.history_scanned_generation=Some(self.registry_generation);
        let docs=catalog.iter().map(|d|(d.name.clone(),RehydratableToolSearchDocument{registration_id:d.registration_id.clone(),source:d.source,allow_lazy_activation:Some(true)})).collect();
        let restored=rehydrate(messages,&docs); if !restored.is_empty() { self.activate_names(&restored,&catalog); } restored
    }
    fn refresh_extension_docs(&mut self) {
        let mut docs:Vec<_>=self.runtime.get_all_tools().iter().filter(|t|t.exposure==ToolExposure::Search && t.allow_lazy_activation).filter_map(extension_document).collect();
        docs.sort_by(|a,b|a.name.cmp(&b.name));
        let fingerprint=serde_json::Value::Array(docs.iter().map(|d|serde_json::json!({"name":d.name,"label":d.label,"aliases":d.aliases,"description":d.description,"searchText":d.search_text,"keywords":d.keywords,"source":d.source,"group":d.group,"ownerLabel":d.owner_label,"registrationId":d.registration_id})).collect()).to_string();
        if fingerprint==self.extension_fingerprint { return; }
        self.extension_fingerprint=fingerprint; self.extension_docs=docs; self.registry_generation=self.registry_generation.wrapping_add(1); self.sync_lifecycle();
    }
    fn sync_lifecycle(&self) {
        let has_docs=!self.extension_docs.is_empty() || !self.mcp_docs.is_empty();
        if has_docs && let Some(register)=&self.registrar { register(); }
        let mut current=self.runtime.get_active_tools(); let active=current.iter().any(|n|n=="tool_search");
        if has_docs==active { return; }
        if has_docs { current.push("tool_search".into()); } else { current.retain(|n|n!="tool_search"); } self.runtime.set_active_tools(&current);
    }
    fn activate_names(&self, names: &[String], catalog: &[ToolSearchDocument]) {
        let mut mcp=Vec::new(); let mut extension=Vec::new();
        for name in names { if let Some(doc)=catalog.iter().rev().find(|d|&d.name==name) { match doc.source { ToolSearchSource::Mcp=>mcp.push(name.clone()), ToolSearchSource::Extension=>extension.push(name.clone()) } } }
        if let Some(hook)=&self.mcp_hook && !mcp.is_empty() { hook(&mcp); }
        if !extension.is_empty() { let mut current=self.runtime.get_active_tools(); extension.retain(|n|!current.contains(n)); extension.sort(); extension.dedup(); current.extend(extension); self.runtime.set_active_tools(&current); }
    }
}
fn extension_document(tool: &ToolInfo) -> Option<ToolSearchDocument> {
    if tool.name.is_empty() { return None; }
    let owner_label=Path::new(&tool.source_info.path).file_stem().and_then(|s|s.to_str()).unwrap_or("").to_owned();
    Some(ToolSearchDocument { name:tool.name.clone(),label:tool.label.clone(),aliases:Vec::new(),description:Some(tool.description.clone()),search_text:tool.search_text.clone(),keywords:tool.search_keywords.clone(),source:ToolSearchSource::Extension,group:tool.search_group.clone().unwrap_or_else(||owner_label.clone()),owner_label,registration_id:derive_extension_registration_id(&tool.source_info.path,None,&tool.name) })
}
