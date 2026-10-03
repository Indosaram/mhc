pub const DEFAULT_LOOK_AT_CHAIN:[&str;4]=["gpt-5.6-terra:off","gemini-3.1-pro-preview:low","gemini-3.5-flash","kimi-k3"];
use maho_ai::{model::Model,types::{InputModality,ModelThinkingLevel,ThinkingLevel}};
use maho_core::model_resolver::{find_exact_model_reference_match,parse_model_pattern};
#[derive(Clone,Debug,PartialEq)]
pub struct ResolvedVisionModel { pub model:Model,pub thinking_level:Option<ThinkingLevel> }
fn normalize(level:Option<ModelThinkingLevel>)->Option<ThinkingLevel> {
    match level { None|Some(ModelThinkingLevel::Off)=>None,Some(ModelThinkingLevel::Minimal)=>Some(ThinkingLevel::Minimal),Some(ModelThinkingLevel::Low)=>Some(ThinkingLevel::Low),Some(ModelThinkingLevel::Medium)=>Some(ThinkingLevel::Medium),Some(ModelThinkingLevel::High)=>Some(ThinkingLevel::High),Some(ModelThinkingLevel::Xhigh)=>Some(ThinkingLevel::Xhigh),Some(ModelThinkingLevel::Max)=>Some(ThinkingLevel::Max) }
}
fn resolve_entry(entry:&str,candidates:&[Model])->Option<ResolvedVisionModel> {
    if let Some(model)=find_exact_model_reference_match(entry,candidates) { return Some(ResolvedVisionModel{model:model.clone(),thinking_level:None}); }
    let (reference,thinking)=entry.rsplit_once(':').and_then(|(reference,suffix)|ModelThinkingLevel::parse(suffix).map(|level|(reference,Some(level)))).unwrap_or((entry,None));
    if let Some(model)=find_exact_model_reference_match(reference,candidates) { return Some(ResolvedVisionModel{model:model.clone(),thinking_level:normalize(thinking)}); }
    if !reference.contains('/') {
        let wanted=reference.trim().to_lowercase();
        let mut same:Vec<_>=candidates.iter().filter(|model|model.id.to_lowercase()==wanted).collect();
        if same.len()>1 {
            for provider in ["openai","google","moonshotai"] { if let Some(model)=same.iter().find(|model|model.provider==provider) { return Some(ResolvedVisionModel{model:(**model).clone(),thinking_level:normalize(thinking)}); } }
            let collator=icu_collator::Collator::try_new(Default::default(),Default::default()).unwrap_or_else(|error|std::panic::panic_any(error));
            same.sort_by(|a,b|collator.compare(&a.provider,&b.provider));
            return Some(ResolvedVisionModel{model:same[0].clone(),thinking_level:normalize(thinking)});
        }
    }
    let parsed=parse_model_pattern(reference,candidates,true);
    parsed.model.map(|model|ResolvedVisionModel{model,thinking_level:normalize(thinking.filter(|level|*level!=ModelThinkingLevel::Off).or(parsed.thinking_level))})
}
pub fn resolve_vision_model(chain:&[String],available:&[Model])->Option<ResolvedVisionModel> {
    let candidates:Vec<_>=available.iter().filter(|model|model.input.contains(&InputModality::Image)).cloned().collect();
    let first=candidates.first()?;
    for entry in chain { if let Some(resolved)=resolve_entry(entry,&candidates) { return Some(resolved); } }
    Some(ResolvedVisionModel{model:first.clone(),thinking_level:None})
}
#[cfg(test)]
mod tests {
    use super::*; use serde_json::json;
    fn model(provider:&str,id:&str,image:bool)->Model { serde_json::from_value(json!({"id":id,"name":id,"provider":provider,"api":"faux","baseUrl":"","reasoning":false,"input":if image {vec!["text","image"]}else{vec!["text"]},"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":4096,"maxTokens":1024})).unwrap() }
    fn chain(entry:&str)->Vec<String> { vec![entry.into()] }
    #[test] fn default_chain_filters_text_only() { let available=vec![model("openai","gpt-5.6-terra",false),model("google","gemini-3.1-pro-preview",true)]; let selected=resolve_vision_model(&DEFAULT_LOOK_AT_CHAIN.map(String::from),&available).unwrap(); assert_eq!(selected.model.provider,"google"); assert_eq!(selected.thinking_level,Some(ThinkingLevel::Low)); }
    #[test] fn text_only_exact_match_is_never_selected() { assert_eq!(resolve_vision_model(&chain("target"),&[model("openai","target",false),model("google","fallback",true)]).unwrap().model.id,"fallback"); }
    #[test] fn off_suffix_becomes_absent() { assert_eq!(resolve_vision_model(&chain("target:off"),&[model("openai","target",true)]).unwrap().thinking_level,None); }
    #[test] fn valid_suffix_is_preserved() { assert_eq!(resolve_vision_model(&chain("target:high"),&[model("openai","target",true)]).unwrap().thinking_level,Some(ThinkingLevel::High)); }
    #[test] fn non_thinking_colon_id_is_exact() { assert_eq!(resolve_vision_model(&chain("target:exacto"),&[model("openai","target:exacto",true)]).unwrap().model.id,"target:exacto"); }
    #[test] fn invalid_thinking_suffix_uses_source_parser_default_not_first_model() {
        let selected=resolve_vision_model(&chain("target:unsupported"),&[model("google","first",true),model("openai","target",true)]).unwrap();
        assert_eq!(selected.model.id,"target");
        assert_eq!(selected.thinking_level,None);
    }
    #[test] fn canonical_reference_beats_ambiguity() { assert_eq!(resolve_vision_model(&chain("moonshotai/shared"),&[model("google","shared",true),model("moonshotai","shared",true)]).unwrap().model.provider,"moonshotai"); }
    #[test] fn preferred_provider_wins_ambiguous_id() { assert_eq!(resolve_vision_model(&chain("shared"),&[model("moonshotai","shared",true),model("google","shared",true),model("openai","shared",true)]).unwrap().model.provider,"openai"); }
    #[test] fn otherwise_alphabetical_provider_wins() { assert_eq!(resolve_vision_model(&chain("shared"),&[model("zebra","shared",true),model("alpha","shared",true)]).unwrap().model.provider,"alpha"); }
    #[test] fn ambiguous_provider_case_uses_source_locale_order() { assert_eq!(resolve_vision_model(&chain("shared"),&[model("Z","shared",true),model("a","shared",true)]).unwrap().model.provider,"a"); }
    #[test] fn fuzzy_reference_uses_shared_parser() { assert_eq!(resolve_vision_model(&chain("gemini-3.5-flash"),&[model("google","gemini-3.5-flash-preview",true)]).unwrap().model.id,"gemini-3.5-flash-preview"); }
    #[test] fn first_vision_is_fallback_and_none_without_vision() { assert_eq!(resolve_vision_model(&chain("missing"),&[model("openai","first",true),model("openai","second",true)]).unwrap().model.id,"first"); assert!(resolve_vision_model(&chain("missing"),&[model("google","text",false)]).is_none()); }
}
