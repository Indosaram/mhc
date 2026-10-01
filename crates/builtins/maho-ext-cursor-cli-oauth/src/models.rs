use std::collections::BTreeSet;
use maho_ai::{cursor::catalog_grouping::{normalize_cursor_catalog,CursorCatalogRawEntry},model::ModelCompat,types::{InputModality,ModelCost}};
use maho_ext_api::types::ProviderModelConfig;
use regex::Regex;
pub fn static_models()->Vec<ProviderModelConfig> {
    parse_models_listing(concat!(
        "auto - Auto\ncomposer-2.5 - Composer 2.5 (200K context)\ncomposer-2.5-fast - Composer 2.5 Fast (200K context)\n",
        "gpt-5.6-sol-high - GPT 5.6 SOL High (272K context)\ngpt-5.6-luna-high - GPT 5.6 Luna High (272K context)\ngpt-5.5-high - GPT 5.5 High (272K context)\n",
        "gpt-5.3-codex - GPT 5.3 Codex (272K context)\ngpt-5.2 - GPT 5.2 (272K context)\n",
        "claude-opus-5-high - Claude Opus 5 High (300K context)\nclaude-opus-5-thinking-high - Claude Opus 5 Thinking High (300K context)\n",
        "claude-opus-4-8-thinking-high - Claude Opus 4.8 Thinking High (300K context)\nclaude-fable-5-thinking-high - Claude Fable 5 Thinking High (300K context)\n",
        "claude-sonnet-5-thinking-high - Claude Sonnet 5 Thinking High (300K context)\ngemini-3.7-flash-high - Gemini 3.7 Flash High (1M context)\ncursor-grok-4.6-high - Cursor Grok 4.6 High (200K context)\n"))
}
fn cached_catalog(contents:&str,now:f64,ttl:f64)->Option<(bool,Vec<ProviderModelConfig>)> {
    let value:serde_json::Value=serde_json::from_str(contents).ok()?;
    let at=value["cachedAt"].as_f64()?;let models=value["models"].as_array()?;
    if !at.is_finite()||models.is_empty()||now<at||now-at>=ttl {return None;}
    if let Some(listing)=value.get("listing") {
        let rebuilt=parse_models_listing(listing.as_str()?);return (!rebuilt.is_empty()).then_some((true,rebuilt));
    }
    let valid=Regex::new(r"^[A-Za-z0-9][A-Za-z0-9._/:+-]*$").expect("constant id regex");let mut seen=BTreeSet::new();let mut raw=Vec::new();
    for entry in models {
        let id=entry["id"].as_str()?;let name=entry["name"].as_str()?;
        if !valid.is_match(id)||name.is_empty()||!seen.insert(id) {return None;}
        raw.push(CursorCatalogRawEntry {id:id.into(),name:name.into(),input:vec!["text".into()],cursor_max_mode:false});
    }
    Some((false,normalize(&raw)))
}
pub async fn resolve_catalog<F,Fut>(agent_dir:&std::path::Path,now:f64,ttl_hours:Option<f64>,probe:F)->Vec<ProviderModelConfig>
where F:FnOnce()->Fut,Fut:std::future::Future<Output=anyhow::Result<String>> {
    let directory=agent_dir.join("cursor-cli-oauth");let path=directory.join("models.json");
    let ttl=ttl_hours.filter(|h|h.is_finite()&&*h>0.0).unwrap_or(24.0)*3600000.0;
    let cached=std::fs::read_to_string(&path).ok().and_then(|text|cached_catalog(&text,now,ttl));
    if let Some((true,models))=cached {return models;}
    let fallback=cached.map_or_else(static_models,|(_,models)|models);
    let Ok(listing)=probe().await else {return fallback;};let models=parse_models_listing(&listing);
    if models.is_empty() {return fallback;}
    let projection:Vec<_>=models.iter().map(|m|serde_json::json!({"id":m.id,"name":m.name})).collect();
    let temporary=path.with_extension(format!("json.{}.{}.tmp",std::process::id(),now));
    let cache=serde_json::json!({"cachedAt":now,"listing":listing,"models":projection});
    if let Ok(text)=serde_json::to_string_pretty(&cache) {
        let written=std::fs::create_dir_all(&directory).and_then(|()|std::fs::write(&temporary,format!("{text}\n"))).and_then(|()|std::fs::rename(&temporary,&path));
        if written.is_err() {let _=std::fs::remove_file(&temporary);}
    }
    models
}
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
    #[tokio::test]
    async fn cache_ttl_and_failed_probe_preserve_listing() {
        let dir=tempfile::tempdir().expect("directory");
        let first=resolve_catalog(dir.path(),1000000.0,Some(2.0),||async {Ok("model-a - Model A\n".into())}).await;assert_eq!(first[0].id,"model-a");
        let fresh=resolve_catalog(dir.path(),4600000.0,Some(2.0),||async {panic!("fresh cache must not probe")}).await;assert_eq!(fresh[0].id,"model-a");
        let expired=resolve_catalog(dir.path(),11800000.0,Some(2.0),||async {Ok("ERROR - unavailable\n".into())}).await;assert_eq!(expired.len(),15);
        let persisted=std::fs::read_to_string(dir.path().join("cursor-cli-oauth/models.json")).expect("cache");assert!(persisted.contains("model-a"));assert!(!persisted.contains("unavailable"));
    }
    #[test]
    fn static_catalog_ids() {assert_eq!(static_models().iter().map(|m|m.id.as_str()).collect::<Vec<_>>(),["auto","composer-2.5","composer-2.5-fast","gpt-5.6-sol","gpt-5.6-luna","gpt-5.5","gpt-5.3-codex","gpt-5.2","claude-opus-5","claude-opus-5-thinking","claude-opus-4-8-thinking","claude-fable-5-thinking","claude-sonnet-5-thinking","gemini-3.7-flash","cursor-grok-4.6"]);}
    #[test]
    fn strips_ansi_ignores_duplicates_and_rejects_misleading_output() {
        let models=parse_models_listing("Available models:\n\u{1b}[36mmodel-a\u{1b}[0m - Model A\nmodel-a - Duplicate\ninvalid model\n");
        assert_eq!(models.len(),1);assert_eq!(models[0].id,"model-a");assert_eq!(models[0].max_tokens,64000);assert_eq!(models[0].input,vec![InputModality::Text]);
        assert!(parse_models_listing("model-a - Model A\nERROR - authentication unavailable\n").is_empty());
    }
}
