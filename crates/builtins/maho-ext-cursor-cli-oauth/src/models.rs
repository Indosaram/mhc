use std::collections::BTreeSet;
use maho_ai::{cursor::catalog_grouping::{normalize_cursor_catalog,CursorCatalogRawEntry},model::ModelCompat,types::{InputModality,ModelCost}};
use maho_ext_api::types::ProviderModelConfig;
use regex::Regex;
fn normalize(raw:&[CursorCatalogRawEntry])->Vec<ProviderModelConfig> {
    normalize_cursor_catalog(raw).into_iter().map(|entry| {
        let mut compat=serde_json::Map::new();
        if let (Some(capability),Some(variant))=(&entry.capability_id,&entry.representative_variant_id) {
            let mut reasoning=serde_json::Map::new();reasoning.insert("capabilityId".into(),serde_json::json!(capability));reasoning.insert("representativeVariantId".into(),serde_json::json!(variant));
            if let Some(mode)=entry.thinking_mode {reasoning.insert("thinkingMode".into(),serde_json::json!(mode));}
            if let Some(variants)=&entry.variant_ids {reasoning.insert("variantIds".into(),serde_json::json!(variants));}
            compat.insert("cursorReasoning".into(),reasoning.into());
        }
        ProviderModelConfig {context_window:maho_ai::utils::cursor_context_limit::resolve_cursor_context_window(&entry.id,entry.window) as u64,
            upstream_model_id:entry.representative_variant_id.filter(|v|v!=&entry.id),id:entry.id,name:entry.name,reasoning:entry.reasoning,thinking_level_map:entry.thinking_level_map,
            input:vec![InputModality::Text],cost:ModelCost {input:0.0,output:0.0,cache_read:0.0,cache_write:0.0,tiers:None},max_tokens:64000,compat:Some(ModelCompat(compat)),
            api:None,base_url:None,recover_text_tool_calls:None,headers:None,extra_body:None}
    }).collect()
}
pub fn parse_models_listing(listing:&str)->Vec<ProviderModelConfig> {
    let ansi=Regex::new("\u{1b}(?:[@-Z\\-_]|\\[[0-?]*[ -/]*[@-~])").expect("constant ANSI regex");
    let plain=ansi.replace_all(listing,"");
    let error=Regex::new(r"(?i)^\s*(?:error|failed|failure)(?:\s|:|-|$)").expect("constant error regex");
    if plain.lines().any(|line|error.is_match(line)) {return Vec::new();}
    let line=Regex::new(r"^(\S+)\s+-\s+(.+)$").expect("constant model regex");
    let id=Regex::new(r"^[A-Za-z0-9][A-Za-z0-9._/:+-]*$").expect("constant id regex");
    let mut seen=BTreeSet::new();let mut raw=Vec::new();
    for value in plain.lines() {
        if let Some(capture)=line.captures(value.trim()) {
            let name=capture[2].trim();let model=&capture[1];
            if id.is_match(model)&&!name.is_empty()&&seen.insert(model.to_owned()) {raw.push(CursorCatalogRawEntry {id:model.into(),name:name.into(),input:vec!["text".into()],cursor_max_mode:false});}
        }
    }
    normalize(&raw)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strips_ansi_ignores_duplicates_and_rejects_misleading_output() {
        let models=parse_models_listing("Available models:\n\u{1b}[36mmodel-a\u{1b}[0m - Model A\nmodel-a - Duplicate\ninvalid model\n");
        assert_eq!(models.len(),1);assert_eq!(models[0].id,"model-a");assert_eq!(models[0].max_tokens,64000);assert_eq!(models[0].input,vec![InputModality::Text]);
        assert!(parse_models_listing("model-a - Model A\nERROR - authentication unavailable\n").is_empty());
    }
}
