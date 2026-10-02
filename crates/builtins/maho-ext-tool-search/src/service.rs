use std::{collections::{BTreeMap,BTreeSet},path::Path,sync::Arc};
use maho_ext_api::{ExtensionActions,ExtensionRuntime,ExtensionFailure,ToolInfo,ToolExposure};
use serde_json::Value;
use crate::{engine::{document::{ToolSearchDocument,ToolSearchSource},bm25::{Bm25Result,Bm25SearchOptions,DEFAULT_BM25_PRECISION,build_bm25_index,normalize_tool_name,tokenize_tool_text},marker::{derive_extension_registration_id,rehydrate,RehydratableToolSearchDocument}},tool::{TOOL_SEARCH_TOOL_NAME,HiddenToolHint}};
pub type FeederActivate=Arc<dyn Fn(&[String])->Result<(),ExtensionFailure>+Send+Sync>;
struct FeedState { docs:Vec<ToolSearchDocument>,activate:FeederActivate }
pub struct ToolSearchService {
    runtime:ExtensionRuntime,actions:Arc<dyn ExtensionActions>,feeds:BTreeMap<ToolSearchSource,FeedState>,
    extension_docs:Vec<ToolSearchDocument>,registry_generation:u64,history_scanned_generation:Option<u64>,
    register_tool_search:Option<Arc<dyn Fn()->Result<(),ExtensionFailure>+Send+Sync>>,
    removed_tool_hints:Arc<dyn Fn()->BTreeMap<String,String>+Send+Sync>,native_injection_failure:Option<String>,
}
impl ToolSearchService {
    pub fn new(runtime:ExtensionRuntime,actions:Arc<dyn ExtensionActions>)->Self {
        Self{runtime,actions,feeds:BTreeMap::new(),extension_docs:vec![],registry_generation:0,history_scanned_generation:None,register_tool_search:None,removed_tool_hints:Arc::new(BTreeMap::new),native_injection_failure:None}
    }
    pub fn bind_runtime(&mut self,runtime:ExtensionRuntime,actions:Arc<dyn ExtensionActions>) { self.runtime=runtime; self.actions=actions; }
    pub fn note_native_injection_failure(&mut self,reason:String) { self.native_injection_failure=Some(reason); }
    pub fn take_native_injection_failure(&mut self)->Option<String> { self.native_injection_failure.take() }
    pub fn bind_tool_registrar(&mut self,register:Arc<dyn Fn()->Result<(),ExtensionFailure>+Send+Sync>) { self.register_tool_search=Some(register); }
    pub fn bind_removed_tool_hints(&mut self,provider:Arc<dyn Fn()->BTreeMap<String,String>+Send+Sync>) { self.removed_tool_hints=provider; }
    pub fn hidden_tool_hints(&self,query:&str)->Vec<HiddenToolHint> {
        let hints=(self.removed_tool_hints)();
        let by_name:BTreeMap<_,_>=hints.iter().map(|(name,hint)|(normalize_tool_name(name),(name,hint))).collect();
        let mut seen=BTreeSet::new();
        std::iter::once(normalize_tool_name(query)).chain(tokenize_tool_text(query)).filter_map(|candidate| {
            let (name,hint)=by_name.get(&candidate)?;
            if !seen.insert(*name) { return None; }
            Some(HiddenToolHint{name:(*name).clone(),hint:(*hint).clone()})
        }).collect()
    }
    pub fn get_tool_parameters(&self,name:&str)->Result<Option<Value>,ExtensionFailure> { Ok(self.actions.get_all_tools()?.into_iter().find(|tool|tool.name==name).map(|tool|tool.parameters)) }
    pub fn begin_session(&mut self)->Result<(),ExtensionFailure> {
        self.feeds.remove(&ToolSearchSource::Mcp); self.registry_generation+=1; self.history_scanned_generation=None;
        self.refresh_extension_docs()?; self.sync_tool_search_lifecycle()
    }
    pub fn feed(&mut self,docs:Vec<ToolSearchDocument>,activate:FeederActivate)->Result<(),ExtensionFailure> {
        self.feeds.insert(ToolSearchSource::Mcp,FeedState{docs:docs.into_iter().filter(|doc|doc.source==ToolSearchSource::Mcp && !doc.name.is_empty() && !doc.registration_id.is_empty()).collect(),activate});
        self.registry_generation+=1; self.sync_tool_search_lifecycle()
    }
    pub fn get_catalog(&mut self)->Result<Vec<ToolSearchDocument>,ExtensionFailure> {
        self.refresh_extension_docs()?;
        Ok(self.feeds.get(&ToolSearchSource::Mcp).into_iter().flat_map(|feed|feed.docs.iter()).chain(&self.extension_docs).cloned().collect())
    }
    pub fn search(&mut self,query:&str,limit:usize,options:&Bm25SearchOptions)->Result<Vec<Bm25Result>,ExtensionFailure> {
        let mut options=options.clone(); if options.precision.is_none() { options.precision=Some(DEFAULT_BM25_PRECISION); }
        Ok(build_bm25_index(&self.get_catalog()?).search(query,limit,&options))
    }
    fn activate_names(&self,names:&[String],catalog:&[ToolSearchDocument])->Result<(),ExtensionFailure> {
        let mut sources:Vec<(ToolSearchSource,Vec<String>)>=vec![];
        for name in names { if let Some(doc)=catalog.iter().rev().find(|doc|doc.name==*name) {
            if let Some((_,group))=sources.iter_mut().find(|(source,_)|*source==doc.source) { group.push(name.clone()); }
            else { sources.push((doc.source,vec![name.clone()])); }
        } }
        for (source,names) in sources {
            match source {
                ToolSearchSource::Mcp=>{if let Some(feed)=self.feeds.get(&source) { (feed.activate)(&names)?; }},
                ToolSearchSource::Extension=>{
                    let actions=self.runtime.session_actions()?; let mut current=actions.get_active_tools()?;
                    let mut added:Vec<_>=names.into_iter().filter(|name|!current.contains(name)).collect();
                    added.sort_by(|left,right|left.encode_utf16().cmp(right.encode_utf16())); added.dedup();
                    current.extend(added); actions.set_active_tools(current)?;
                },
            }
        }
        Ok(())
    }
    pub fn activate(&self,matches:&[Bm25Result])->Result<Vec<String>,ExtensionFailure> {
        let names:Vec<_>=matches.iter().map(|m|m.name.clone()).collect(); let catalog:Vec<_>=matches.iter().map(|m|m.doc.clone()).collect(); self.activate_names(&names,&catalog)?;
        let active=self.runtime.session_actions()?.get_active_tools()?; Ok(names.into_iter().filter(|name|active.contains(name)).collect())
    }
    pub fn activate_tool(&mut self,name:&str)->Result<bool,ExtensionFailure> {
        let catalog=self.get_catalog()?; if !catalog.iter().any(|doc|doc.name==name) { return Ok(false); }
        self.activate_names(&[name.into()],&catalog)?; Ok(self.runtime.session_actions()?.get_active_tools()?.iter().any(|active|active==name))
    }
    pub fn maybe_rehydrate_from_history(&mut self,messages:&[Value])->Result<Vec<String>,ExtensionFailure> {
        let catalog=self.get_catalog()?; if self.history_scanned_generation==Some(self.registry_generation) { return Ok(vec![]); }
        self.history_scanned_generation=Some(self.registry_generation);
        let docs=catalog.iter().map(|doc|(doc.name.clone(),RehydratableToolSearchDocument{registration_id:doc.registration_id.clone(),source:doc.source,allow_lazy_activation:Some(true)})).collect();
        let restored=rehydrate(messages,&docs); if !restored.is_empty() { self.activate_names(&restored,&catalog)?; } Ok(restored)
    }
    fn refresh_extension_docs(&mut self)->Result<(),ExtensionFailure> {
        let mut docs:Vec<_>=self.actions.get_all_tools()?.into_iter().filter(|tool|tool.exposure==ToolExposure::Search && tool.allow_lazy_activation).filter_map(extension_document).collect();
        docs.sort_by(|a,b|a.name.cmp(&b.name));
        if docs==self.extension_docs { return Ok(()); }
        self.extension_docs=docs; self.registry_generation+=1; self.sync_tool_search_lifecycle()
    }
    fn sync_tool_search_lifecycle(&self)->Result<(),ExtensionFailure> {
        let has_documents=!self.extension_docs.is_empty() || self.feeds.get(&ToolSearchSource::Mcp).is_some_and(|feed|!feed.docs.is_empty());
        if has_documents && let Some(register)=&self.register_tool_search { register()?; }
        let actions=self.runtime.session_actions()?; let mut current=actions.get_active_tools()?;
        let active=current.iter().any(|name|name==TOOL_SEARCH_TOOL_NAME); if active==has_documents { return Ok(()); }
        if has_documents { current.push(TOOL_SEARCH_TOOL_NAME.into()); } else { current.retain(|name|name!=TOOL_SEARCH_TOOL_NAME); }
        actions.set_active_tools(current)
    }
}
fn extension_document(tool:ToolInfo)->Option<ToolSearchDocument> {
    if tool.name.is_empty() { return None; }
    let file=Path::new(&tool.source_info.path).file_name().unwrap_or_else(||std::ffi::OsStr::new("")).to_string_lossy();
    let owner=Path::new(file.as_ref()).file_stem().unwrap_or_else(||std::ffi::OsStr::new(file.as_ref())).to_string_lossy().into_owned();
    Some(ToolSearchDocument{registration_id:derive_extension_registration_id(&tool.source_info.path,None,&tool.name),name:tool.name,label:tool.label,aliases:vec![],description:Some(tool.description),search_text:tool.search_text,keywords:tool.search_keywords,source:ToolSearchSource::Extension,group:tool.search_group.unwrap_or_else(||owner.clone()),owner_label:owner})
}
#[cfg(test)]
mod tests {
    use super::*;
    use maho_ext_api::{CustomMessage,SendMessageOptions,UserMessageContent,SendUserMessageOptions,SourceInfo};
    struct Catalog(Vec<ToolInfo>);
    impl ExtensionActions for Catalog {
        fn send_message(&self,_message:CustomMessage,_options:SendMessageOptions)->Result<(),ExtensionFailure> { Err(ExtensionFailure::new("not used")) }
        fn send_user_message(&self,_content:UserMessageContent,_options:SendUserMessageOptions)->Result<(),ExtensionFailure> { Err(ExtensionFailure::new("not used")) }
        fn append_entry(&self,_kind:&str,_data:Option<Value>)->Result<(),ExtensionFailure> { Err(ExtensionFailure::new("not used")) }
        fn get_all_tools(&self)->Result<Vec<ToolInfo>,ExtensionFailure> { Ok(self.0.clone()) }
    }
    fn service()->ToolSearchService { ToolSearchService::new(ExtensionRuntime::default(),Arc::new(Catalog(vec![]))) }
    #[test] fn activation_preserves_source_encounter_order() {
        let mut service=service();
        let called=Arc::new(std::sync::atomic::AtomicBool::new(false)); let observed=called.clone();
        service.feeds.insert(ToolSearchSource::Mcp,FeedState{docs:vec![],activate:Arc::new(move |_| {observed.store(true,std::sync::atomic::Ordering::SeqCst);Ok(())})});
        let doc=|name:&str,source|ToolSearchDocument{name:name.into(),label:name.into(),aliases:vec![],description:None,search_text:None,keywords:vec![],source,group:String::new(),owner_label:String::new(),registration_id:name.into()};
        let catalog=vec![doc("extension",ToolSearchSource::Extension),doc("mcp",ToolSearchSource::Mcp)];
        assert!(service.activate_names(&["extension".into(),"mcp".into()],&catalog).is_err());
        assert!(!called.load(std::sync::atomic::Ordering::SeqCst),"later MCP source must not activate after the earlier extension failure");
    }
    #[test] fn lifecycle_hooks_register_without_eager_tool_registration() {
        let mut api=maho_ext_api::ExtensionApi::new(maho_ext_api::LoadedExtension::new("tool-search",Default::default(),Default::default()),Default::default(),Default::default(),Default::default());
        crate::index::register_session_hooks(&mut api,Arc::new(std::sync::Mutex::new(service())));
        assert_eq!(api.registered.handlers[&maho_ext_api::EventKind::SessionStart].len(),1);
        assert_eq!(api.registered.handlers[&maho_ext_api::EventKind::Context].len(),1);
        assert!(api.registered.tools.is_empty());
    }
    #[test] fn native_failure_is_consumed_once() { let mut service=service(); service.note_native_injection_failure("rejected".into()); assert_eq!(service.take_native_injection_failure(),Some("rejected".into())); assert_eq!(service.take_native_injection_failure(),None); }
    #[test] fn hidden_hints_follow_query_order_without_duplicates() {
        let mut service=service(); service.bind_removed_tool_hints(Arc::new(||BTreeMap::from([("bash".into(),"use eval".into()),("write".into(),"use patch".into())])));
        assert_eq!(service.hidden_tool_hints("write bash write"),vec![HiddenToolHint{name:"write".into(),hint:"use patch".into()},HiddenToolHint{name:"bash".into(),hint:"use eval".into()}]);
    }
    #[test] fn missing_parameters_are_absent() { assert_eq!(service().get_tool_parameters("unknown").unwrap(),None); }
    #[test] fn empty_source_path_keeps_named_extension_document() {
        let doc=extension_document(ToolInfo{name:"read".into(),label:"Read".into(),description:String::new(),parameters:Value::Null,prompt_guidelines:None,source_info:SourceInfo::default(),exposure:ToolExposure::Search,search_text:None,search_keywords:vec![],search_group:None,allow_lazy_activation:true}).unwrap();
        assert_eq!(doc.owner_label,""); assert_eq!(doc.group,""); assert_eq!(doc.registration_id,"\0read");
    }
    #[test] fn empty_catalog_search_has_no_matches() { assert!(service().search("files",10,&Bm25SearchOptions::default()).unwrap().is_empty()); }
    #[test] fn native_executor_returns_machine_details_for_empty_catalog() {
        let tool=crate::tool::create_tool_search_tool(Arc::new(std::sync::Mutex::new(service())));
        let mut future=(tool.execute)(maho_tools::definition::ToolCall{id:"test",params:serde_json::json!({"query":"files"}),signal:Default::default(),on_update:None,context:None});
        let waker=std::task::Waker::noop(); let mut context=std::task::Context::from_waker(waker);
        let std::task::Poll::Ready(result)=future.as_mut().poll(&mut context) else { panic!("synchronous catalog search must settle without external IO") };
        let details=result.unwrap().details.unwrap(); assert_eq!(details["query"],"files"); assert_eq!(details["matched"],serde_json::json!([]));
    }
    #[test] fn unbound_activation_is_explicit_failure() { assert!(service().begin_session().is_err()); }
    #[test] fn extension_document_uses_owner_and_group() {
        let doc=extension_document(ToolInfo{name:"search_docs".into(),label:"Docs".into(),description:"Search documentation".into(),parameters:Value::Null,prompt_guidelines:None,source_info:SourceInfo{path:"/extensions/docs.ts".into(),..Default::default()},exposure:ToolExposure::Search,search_text:None,search_keywords:vec![],search_group:None,allow_lazy_activation:true}).unwrap();
        assert_eq!(doc.owner_label,"docs"); assert_eq!(doc.group,"docs"); assert_eq!(doc.registration_id,"/extensions/docs.ts\0search_docs");
    }
}
