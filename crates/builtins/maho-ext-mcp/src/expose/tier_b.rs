//! Tier-B adaptive exposure wiring: search-mode registration, stub swap,
//! promotion, and the shared-catalog feed used by `exposure:"auto"`.
use std::{collections::{BTreeMap,BTreeSet},sync::{Arc,Mutex}};
use maho_ext_api::{ExtensionFailure,ToolDefinition,ToolExecutionMode,ToolResult};
use maho_ext_tool_search::engine::document::{ToolSearchDocument,ToolSearchSource};
use maho_ext_tool_search::engine::marker::derive_mcp_registration_id;
use maho_ext_tool_search::service::{FeederActivate,ToolSearchService};
use serde_json::json;

use crate::active_set::register_tools_preserving_active_set_with;
use crate::catalog::McpToolCatalogEntry;
use crate::config_schema::{McpSettings,OutputGuardSettings};
use crate::guard::output_guard::McpOutputArtifacts;
use crate::tool_registrar::McpToolRegistrar;
use super::proxy::create_mcp_proxy_tool;
use super::register::{build_mcp_tool_definitions,map_mcp_catalog_names};

fn poisoned<T>(lock:&Mutex<T>)->std::sync::MutexGuard<'_,T> {lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner)}

pub fn order_active_set(names: &[String], reference: &[String], catalog_names: &BTreeSet<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let unique = names.iter().filter(|name| seen.insert((*name).clone())).cloned().collect::<Vec<_>>();
    let rank = reference.iter().enumerate().map(|(index, name)| (name, index)).collect::<BTreeMap<_, _>>();
    let mut base = unique.iter().filter(|name| !catalog_names.contains(*name)).cloned().collect::<Vec<_>>();
    base.sort_by_key(|name| rank.get(name).copied().unwrap_or(usize::MAX));
    let mut catalog = unique.into_iter().filter(|name| catalog_names.contains(name)).collect::<Vec<_>>();
    catalog.sort_by_cached_key(|name| name.encode_utf16().collect::<Vec<_>>());
    base.extend(catalog); base
}

pub struct McpStubPromotion {
    pub full: ToolDefinition,
    pub promote: Arc<dyn Fn() -> Result<(),ExtensionFailure> + Send + Sync>,
}

pub fn build_mcp_stub_definition(name: &str, promotion: Option<McpStubPromotion>) -> ToolDefinition {
    let description = if promotion.is_some() {
        format!("Deferred MCP tool. Call {name} with its real arguments; it activates and runs on this first call (tool_search lists its schema).")
    } else {
        format!("Inactive MCP tool. Run tool_search to activate {name}, then call it on your next turn.")
    };
    let name_owned = name.to_owned();
    let promotion = promotion.map(Arc::new);
    let mut definition = ToolDefinition::new(name, &description, json!({"type":"object","properties":{},"additionalProperties":true}), Arc::new(move |call| {
        let promotion = promotion.clone(); let name = name_owned.clone();
        Box::pin(async move {
            match promotion {
                Some(promotion) => {(promotion.promote)().map_err(|error|maho_ext_api::ToolError::Message(error.to_string()))?; (promotion.full.execute)(call).await}
                None => Ok(ToolResult::text(format!("{name} is not active. Use tool_search to activate it, then call it."))),
            }
        })
    }));
    definition.execution_mode = Some(ToolExecutionMode::Parallel); definition
}

pub struct SearchableMcpTool {pub name:String,pub tool_name:String,pub description:Option<String>,pub server:String}

pub struct McpTierBRegistrationInput {
    pub registered_entries:Vec<McpToolCatalogEntry>,
    pub active_entries:Vec<McpToolCatalogEntry>,
    pub search_mode:bool,
    pub proxy_gateways:Vec<(String,Vec<McpToolCatalogEntry>)>,
    pub utility_tools:Vec<ToolDefinition>,
    pub settings:McpSettings,
    pub agent_dir:std::path::PathBuf,
    pub artifacts:Arc<McpOutputArtifacts>,
    pub output_guard:Option<OutputGuardSettings>,
}

pub struct McpTierBRegistration {pub searchable:Vec<SearchableMcpTool>,pub activate:FeederActivate}

#[derive(Default)]
pub struct McpTierBRegistry {pub managed_names:BTreeSet<String>,pub promoted_names:Option<Arc<Mutex<BTreeSet<String>>>>}

fn is_legacy_mcp_registration_name(name:&str)->bool {name.starts_with("mcp_")}
fn union_stable(current:&[String],added:&[String])->Vec<String> {let mut seen=BTreeSet::new();current.iter().chain(added.iter()).filter(|name|seen.insert((*name).clone())).cloned().collect()}

pub fn register_mcp_tier_b_tools(registrar:Arc<dyn McpToolRegistrar>,input:McpTierBRegistrationInput,tool_search:Option<&mut ToolSearchService>,registry:&mut McpTierBRegistry,warn:Option<Arc<dyn Fn(String)+Send+Sync>>)->Result<McpTierBRegistration,ExtensionFailure> {
    let _=warn;
    if input.search_mode && tool_search.is_none() {
        return Err(ExtensionFailure::new("MCP search-mode exposure requires the shared tool_search service; the host tool-search wiring is unresolved, so no MCP catalog was fed or activated."));
    }
    let named=map_mcp_catalog_names(&input.registered_entries);
    let searchable:Vec<SearchableMcpTool>=named.iter().map(|named|SearchableMcpTool {name:named.name.clone(),tool_name:named.entry.tool.clone(),description:named.entry.description.clone(),server:named.entry.server.clone()}).collect();
    let documents:Vec<ToolSearchDocument>=named.iter().map(|named|ToolSearchDocument {name:named.name.clone(),label:named.entry.tool.clone(),aliases:vec![named.entry.tool.clone()],description:named.entry.description.clone(),search_text:None,keywords:Vec::new(),source:ToolSearchSource::Mcp,group:named.entry.server.clone(),owner_label:named.entry.server.clone(),registration_id:derive_mcp_registration_id(&named.entry.server,&named.entry.tool)}).collect();
    let full_defs=build_mcp_tool_definitions(&input.registered_entries,input.agent_dir.clone(),input.artifacts.clone(),input.output_guard.clone());
    let full_by_name:Arc<BTreeMap<String,ToolDefinition>>=Arc::new(full_defs.iter().map(|definition|(definition.name.clone(),definition.clone())).collect());
    let mut gateway_names:Vec<String>=Vec::new();
    for (server,entries) in &input.proxy_gateways {
        let tool=create_mcp_proxy_tool(server,entries,input.agent_dir.clone(),input.artifacts.clone(),input.output_guard.clone());
        registrar.register_tool(tool.clone())?; gateway_names.push(tool.name);
    }
    for tool in &input.utility_tools {registrar.register_tool(tool.clone())?; gateway_names.push(tool.name.clone());}
    let active_mcp_names:Vec<String>=map_mcp_catalog_names(&input.active_entries).into_iter().map(|named|named.name).chain(gateway_names.iter().cloned()).collect();
    let managed_names:BTreeSet<String>=full_defs.iter().map(|definition|definition.name.clone()).chain(gateway_names.iter().cloned()).collect();
    let previous_managed_names=std::mem::take(&mut registry.managed_names);
    registry.managed_names=managed_names.clone();
    let stub_swap=input.search_mode && input.settings.stub_swap==Some(true);
    let registered_names:Arc<BTreeSet<String>>=Arc::new(full_defs.iter().map(|definition|definition.name.clone()).collect());
    let catalog_names:Arc<Mutex<BTreeSet<String>>>=Arc::new(Mutex::new(BTreeSet::new()));
    let stubbed:Arc<Mutex<BTreeSet<String>>>=Arc::new(Mutex::new(BTreeSet::new()));
    let promoted:Arc<Mutex<BTreeSet<String>>>=registry.promoted_names.get_or_insert_with(||Arc::new(Mutex::new(BTreeSet::new()))).clone();
    let activate:FeederActivate={let registrar=registrar.clone();let full_by_name=full_by_name.clone();let registered_names=registered_names.clone();let catalog_names=catalog_names.clone();let stubbed=stubbed.clone();let promoted=promoted.clone();
        Arc::new(move |names:&[String]| {
            let mut seen=BTreeSet::new();
            let known:Vec<String>=names.iter().filter(|name|registered_names.contains(*name) && seen.insert((*name).clone())).cloned().collect();
            if known.is_empty() {return Ok(());}
            if stub_swap {
                for name in &known {
                    let is_stub=poisoned(&stubbed).contains(name);
                    if is_stub && let Some(full)=full_by_name.get(name) {
                        registrar.register_tool(full.clone())?;
                        poisoned(&stubbed).remove(name);
                        poisoned(&promoted).insert(name.clone());
                    }
                }
            }
            let current=registrar.get_active_tools()?;
            let catalog=poisoned(&catalog_names).clone();
            registrar.set_active_tools(order_active_set(&union_stable(&current,&known),&current,&catalog))
        })};
    if let Some(service)=tool_search {
        service.feed(if input.search_mode {documents} else {Vec::new()},activate.clone())?;
        for document in service.get_catalog()? {poisoned(&catalog_names).insert(document.name);}
    }
    let reference=registrar.get_active_tools()?;
    let current_base:Vec<String>=reference.iter().filter(|name|!is_legacy_mcp_registration_name(name) && !previous_managed_names.contains(*name) && !managed_names.contains(*name)).cloned().collect();
    if !input.search_mode {
        let intended=order_active_set(&current_base.iter().cloned().chain(active_mcp_names.iter().cloned()).collect::<Vec<_>>(),&reference,&poisoned(&catalog_names));
        register_tools_preserving_active_set_with(registrar.as_ref(),full_defs,Some(intended))?;
        return Ok(McpTierBRegistration {searchable,activate});
    }
    if !stub_swap {
        let intended=order_active_set(&current_base.iter().cloned().chain(active_mcp_names.iter().cloned()).collect::<Vec<_>>(),&reference,&poisoned(&catalog_names));
        register_tools_preserving_active_set_with(registrar.as_ref(),full_defs,Some(intended))?;
        return Ok(McpTierBRegistration {searchable,activate});
    }
    let direct_active:BTreeSet<String>=active_mcp_names.iter().cloned().collect();
    let promoted_snapshot:BTreeSet<String>=poisoned(&promoted).clone();
    let mut to_register:Vec<ToolDefinition>=Vec::new();
    for definition in full_defs {
        if direct_active.contains(&definition.name) || promoted_snapshot.contains(&definition.name) {to_register.push(definition);continue;}
        poisoned(&stubbed).insert(definition.name.clone());
        let activate=activate.clone();let stub_name=definition.name.clone();let promote_name=definition.name.clone();
        to_register.push(build_mcp_stub_definition(&stub_name,Some(McpStubPromotion {full:definition,promote:Arc::new(move || activate(std::slice::from_ref(&promote_name)))})));
    }
    let all_mcp_names:Vec<String>=full_by_name.keys().cloned().collect();
    let intended=order_active_set(&current_base.iter().cloned().chain(all_mcp_names).collect::<Vec<_>>(),&reference,&poisoned(&catalog_names));
    register_tools_preserving_active_set_with(registrar.as_ref(),to_register,Some(intended))?;
    Ok(McpTierBRegistration {searchable,activate})
}
