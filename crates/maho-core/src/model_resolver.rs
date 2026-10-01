use maho_ai::types::{Model, ModelThinkingLevel, ThinkingSelection, ThinkingSelectionSource};
use maho_ai::cursor::model_capabilities::get_cursor_variant_alias;

#[derive(Debug, Clone, Default)]
pub struct ParsedModelResult {
    pub model: Option<Model>,
    pub thinking_level: Option<ModelThinkingLevel>,
    pub thinking_selection: Option<ThinkingSelection>,
    pub service_tier: Option<String>,
    pub warning: Option<String>,
}

pub fn get_model_narrowing_patterns<'a>(cli: Option<&'a [String]>, legacy: Option<&'a [String]>) -> &'a [String] {
    cli.or(legacy).unwrap_or(&[])
}

pub fn find_exact_model_reference_match<'a>(reference: &str, models: &'a [Model]) -> Option<&'a Model> {
    let reference = reference.trim();
    if reference.is_empty() { return None; }
    let canonical: Vec<_> = models.iter().filter(|m| format!("{}/{}", m.provider, m.id).eq_ignore_ascii_case(reference)).collect();
    match canonical.as_slice() { [model] => return Some(model), [] => {}, _ => return None }
    if let Some((provider, id)) = reference.split_once('/') {
        let matches: Vec<_> = models.iter().filter(|m| m.provider.eq_ignore_ascii_case(provider.trim()) && m.id.eq_ignore_ascii_case(id.trim())).collect();
        match matches.as_slice() { [model] => return Some(model), [] => {}, _ => return None }
    }
    let matches: Vec<_> = models.iter().filter(|m| m.id.eq_ignore_ascii_case(reference)).collect();
    match matches.as_slice() { [model] => Some(model), _ => None }
}

fn legacy_resolution(reference: &str, models: &[Model]) -> Option<ParsedModelResult> {
    let reference = reference.trim();
    let (provider, variant) = reference.split_once('/').map_or((None, reference), |(p, id)| (Some(p), id));
    let eligible = |model: &&Model| matches!(model.provider.as_str(), "cursor" | "cursor-cli-oauth") && provider.is_none_or(|p| model.provider.eq_ignore_ascii_case(p));
    if let Some(alias) = get_cursor_variant_alias(variant) {
        let candidates: Vec<_> = models.iter().filter(eligible).filter(|m| m.id == alias.target_id).collect();
        let [model] = candidates.as_slice() else { return None; };
        return Some(ParsedModelResult { model: Some((*model).clone()), thinking_level: alias.level,
            thinking_selection: alias.level.map(|level| ThinkingSelection { level, source: ThinkingSelectionSource::LegacyVariant, legacy_variant_id: Some(variant.into()) }), ..Default::default() });
    }
    let candidates: Vec<_> = models.iter().filter(|m| provider.is_none() || eligible(m)).cloned().collect();
    if find_exact_model_reference_match(reference, &candidates).is_some() { return None; }
    let mut derived = Vec::new();
    for model in models.iter().filter(eligible) {
        if let Some(variants) = model.compat.as_ref().and_then(|c| c.0.get("cursorReasoning")).and_then(|v| v.get("variantIds")).and_then(serde_json::Value::as_object) {
            for (level, id) in variants {
                if let (Some(level), Some(id)) = (ModelThinkingLevel::parse(level), id.as_str()) && id.eq_ignore_ascii_case(variant) {
                    derived.push(ParsedModelResult { model: Some(model.clone()), thinking_level: Some(level), thinking_selection: Some(ThinkingSelection {
                        level, source: ThinkingSelectionSource::LegacyVariant, legacy_variant_id: Some(id.into()),
                    }), ..Default::default() });
                }
            }
        }
    }
    if derived.len() == 1 { derived.pop() } else { None }
}

fn is_alias(id: &str) -> bool {
    id.ends_with("-latest") || !id.rsplit_once('-').is_some_and(|(_, suffix)| suffix.len() == 8 && suffix.bytes().all(|b| b.is_ascii_digit()))
}

fn try_match_model<'a>(pattern: &str, models: &'a [Model]) -> Option<&'a Model> {
    if let Some(model) = find_exact_model_reference_match(pattern, models) { return Some(model); }
    let pattern = pattern.to_lowercase();
    let mut matches: Vec<_> = models.iter().filter(|m| m.id.to_lowercase().contains(&pattern) || m.name.to_lowercase().contains(&pattern)).collect();
    matches.sort_by(|a, b| is_alias(&b.id).cmp(&is_alias(&a.id)).then_with(|| b.id.cmp(&a.id)));
    matches.first().copied()
}

pub fn parse_model_pattern(pattern: &str, models: &[Model], allow_invalid_thinking_level_fallback: bool) -> ParsedModelResult {
    if let Some(result) = legacy_resolution(pattern, models) { return result; }
    if let Some(model) = try_match_model(pattern, models) { return ParsedModelResult { model: Some(model.clone()), ..Default::default() }; }
    let Some((prefix, suffix)) = pattern.rsplit_once(':') else { return ParsedModelResult::default(); };
    let thinking = ModelThinkingLevel::parse(suffix);
    let tier = matches!(suffix, "auto" | "flex" | "priority");
    if thinking.is_none() && !tier && !allow_invalid_thinking_level_fallback { return ParsedModelResult::default(); }
    let mut result = parse_model_pattern(prefix, models, allow_invalid_thinking_level_fallback);
    if result.model.is_none() { return result; }
    if let Some(level) = thinking {
        result.thinking_level = if result.warning.is_some() { None } else { result.thinking_level.or(Some(level)) };
        result.thinking_selection = result.thinking_level.map(|level| ThinkingSelection { level, source: ThinkingSelectionSource::Explicit, legacy_variant_id: None });
    } else if tier {
        result.service_tier = if result.warning.is_some() { None } else { result.service_tier.or_else(|| Some(suffix.into())) };
    } else {
        result.thinking_level = None;
        result.thinking_selection = None;
        result.warning = Some(format!("Invalid thinking level \"{suffix}\" in pattern \"{pattern}\". Using default instead."));
    }
    result
}

pub fn resolve_stored_model_reference(provider: &str, id: &str, models: &[Model]) -> Option<ParsedModelResult> {
    legacy_resolution(&format!("{provider}/{id}"), models).or_else(|| {
        models.iter().find(|m| m.provider == provider && m.id == id).map(|m| ParsedModelResult { model: Some(m.clone()), ..Default::default() })
    })
}

#[derive(Debug,Clone)]
pub struct ScopedModel {pub model:Model,pub thinking_level:Option<ModelThinkingLevel>,pub thinking_selection:Option<ThinkingSelection>,pub service_tier:Option<String>}
#[derive(Debug,Clone)]
pub struct ModelScopeDiagnostic {pub code:&'static str,pub message:String,pub pattern:String}
#[derive(Debug,Clone)]
pub struct PatternResolution {pub pattern:String,pub owned_ids:Vec<String>,pub thinking_level:Option<ModelThinkingLevel>,pub service_tier:Option<String>,pub unresolved:bool,pub is_glob:bool}
#[derive(Debug,Default)]
pub struct ResolveModelScopeResult {pub scoped_models:Vec<ScopedModel>,pub diagnostics:Vec<ModelScopeDiagnostic>,pub pattern_resolutions:Vec<PatternResolution>}

pub fn resolve_model_scope_from_models(patterns:&[String],models:&[Model])->ResolveModelScopeResult {
    let mut result=ResolveModelScopeResult::default();let mut claimed=std::collections::HashSet::new();
    for pattern in patterns {
        let is_glob=pattern.contains(['*','?','[']);let mut matches=Vec::new();let mut thinking=None;let mut tier=None;
        if is_glob {
            let mut glob=pattern.as_str();
            while let Some((prefix,suffix))=glob.rsplit_once(':') {
                if thinking.is_none()&&ModelThinkingLevel::parse(suffix).is_some(){thinking=ModelThinkingLevel::parse(suffix);}
                else if tier.is_none()&&matches!(suffix,"auto"|"flex"|"priority"){tier=Some(suffix.to_owned());}else{break;}
                glob=prefix;
            }
            if let Some(model)=find_exact_model_reference_match(glob,models){matches.push(ParsedModelResult{model:Some(model.clone()),thinking_level:thinking,service_tier:tier.clone(),..Default::default()});}
            else {
                let matcher=compile_glob(glob);
                if let Ok(matcher)=matcher {
                    for model in models {
                        if matcher.is_match(&format!("{}/{}",model.provider,model.id))||(!glob.contains('/')&&matcher.is_match(&model.id)) {
                            matches.push(ParsedModelResult{model:Some(model.clone()),thinking_level:thinking,service_tier:tier.clone(),..Default::default()});
                        }
                    }
                }
            }
        } else {
            let parsed=parse_model_pattern(pattern,models,true);thinking=parsed.thinking_level;tier=parsed.service_tier.clone();
            if let Some(warning)=&parsed.warning{result.diagnostics.push(ModelScopeDiagnostic{code:"invalid-thinking-level",message:warning.clone(),pattern:pattern.clone()});}
            if parsed.model.is_some(){matches.push(parsed);}
        }
        let unresolved=matches.is_empty();let mut owned_ids=Vec::new();
        if unresolved{result.diagnostics.push(ModelScopeDiagnostic{code:"no-match",message:format!("No models match pattern \"{pattern}\""),pattern:pattern.clone()});}
        for parsed in matches {
            let Some(model)=parsed.model else{continue;};let id=format!("{}/{}",model.provider,model.id);
            if claimed.insert(id.clone()) {
                owned_ids.push(id);let selection=parsed.thinking_selection.or_else(||parsed.thinking_level.map(|level|ThinkingSelection{level,source:ThinkingSelectionSource::Explicit,legacy_variant_id:None}));
                result.scoped_models.push(ScopedModel{model,thinking_level:parsed.thinking_level,thinking_selection:selection,service_tier:parsed.service_tier});
            }
        }
        result.pattern_resolutions.push(PatternResolution{pattern:pattern.clone(),owned_ids,thinking_level:thinking,service_tier:tier,unresolved,is_glob});
    }
    result
}

fn compile_glob(pattern:&str)->Result<regex::Regex,regex::Error> {
    let mut expression=String::from("(?i)^");let mut chars=pattern.chars().peekable();
    while let Some(ch)=chars.next() {
        match ch {
            '*'=>expression.push_str(".*"),'?'=>expression.push('.'),
            '['=>{expression.push('[');if chars.peek()==Some(&'!'){chars.next();expression.push('^');}for ch in chars.by_ref(){expression.push(ch);if ch==']'{break;}}},
            ch=>expression.push_str(&regex::escape(&ch.to_string())),
        }
    }
    expression.push('$');regex::Regex::new(&expression)
}

#[derive(Debug,Default)]
pub struct ResolveCliModelResult {pub parsed:ParsedModelResult,pub error:Option<String>}

pub const DEFAULT_MODEL_PER_PROVIDER:&[(&str,&str)]=&[
    ("alibaba-token-plan","qwen3.7-max"),("amazon-bedrock","us.anthropic.claude-opus-4-6-v1"),("ant-ling","Ring-2.6-1T"),("anthropic","claude-opus-4-8"),("bai","gpt-5.6-sol"),("openai","gpt-6-sol"),("azure-openai-responses","gpt-5.4"),("chatgpt-subscription","gpt-6-sol"),("ollama","qwen3.5:397b"),("cursor","auto"),("radius","balanced"),("nvidia","nvidia/nemotron-3-super-120b-a12b"),("deepseek","deepseek-v4-pro"),("google","gemini-3.1-pro-preview"),("google-vertex","gemini-3.1-pro-preview"),("github-copilot","gpt-5.4"),("openrouter","moonshotai/kimi-k2.6"),("vercel-ai-gateway","zai/glm-5.1"),("opengateway","moonshotai/kimi-k3"),("xai","grok-4.7"),("groq","openai/gpt-oss-120b"),("cerebras","gpt-oss-120b"),("zai","glm-5.3"),("zai-coding-cn","glm-5.3"),("mistral","devstral-medium-latest"),("minimax","MiniMax-M2.7"),("minimax-cn","MiniMax-M2.7"),("moonshotai","kimi-k2.6"),("moonshotai-cn","kimi-k2.6"),("huggingface","moonshotai/Kimi-K2.6"),("fireworks","accounts/fireworks/models/kimi-k2p6"),("together","moonshotai/Kimi-K2.6"),("venice","z-ai-glm-5-3"),("baseten","zai-org/GLM-5.2"),("opencode","kimi-k2.6"),("opencode-go","kimi-k2.6"),("kimi-coding","kimi-for-coding"),("cloudflare-workers-ai","@cf/moonshotai/kimi-k2.6"),("cloudflare-ai-gateway","workers-ai/@cf/moonshotai/kimi-k2.6"),("qwen-token-plan","qwen3.7-max"),("qwen-token-plan-cn","qwen3.7-max"),("qwen-token-plan-individual","qwen3.8-max"),("xiaomi","mimo-v2.5-pro"),("xiaomi-token-plan-cn","mimo-v2.5-pro"),("xiaomi-token-plan-ams","mimo-v2.5-pro"),("xiaomi-token-plan-sgp","mimo-v2.5-pro"),
];
fn preferred_available(models:&[Model])->Option<Model> {DEFAULT_MODEL_PER_PROVIDER.iter().find_map(|(p,id)|models.iter().find(|m|m.provider==*p&&m.id==*id).cloned()).or_else(||models.first().cloned())}

#[derive(Debug,Clone,Copy,PartialEq,Eq)]
pub enum InitialModelProvenance {Cli,Scoped,Settings,ProviderDefault,FirstAvailable}
pub struct InitialModelResult {pub parsed:ParsedModelResult,pub fallback_message:Option<String>,pub provenance:InitialModelProvenance}
pub struct InitialModelOptions<'a> {
    pub cli_provider:Option<&'a str>,pub cli_model:Option<&'a str>,pub scoped_models:&'a [ScopedModel],pub is_continuing:bool,
    pub default_provider:Option<&'a str>,pub default_model_id:Option<&'a str>,pub model_thinking_levels:Option<&'a std::collections::HashMap<String,ModelThinkingLevel>>,
}
pub async fn find_initial_model(options:InitialModelOptions<'_>,runtime:&crate::model_runtime::ModelRuntime)->Result<InitialModelResult,String> {
    if options.cli_provider.is_some()&&options.cli_model.is_some() {
        let resolved=resolve_cli_model(options.cli_provider,options.cli_model,None,runtime);if let Some(error)=resolved.error{return Err(error);}
        if resolved.parsed.model.is_some(){return Ok(InitialModelResult{parsed:resolved.parsed,fallback_message:None,provenance:InitialModelProvenance::Cli});}
    }
    if !options.is_continuing&&let Some(scoped)=options.scoped_models.first() {
        let thinking=options.model_thinking_levels.and_then(|v|v.get(&format!("{}/{}",scoped.model.provider,scoped.model.id))).copied().or(scoped.thinking_level);
        return Ok(InitialModelResult{parsed:ParsedModelResult{model:Some(scoped.model.clone()),thinking_level:thinking,thinking_selection:scoped.thinking_selection.clone(),..Default::default()},fallback_message:None,provenance:InitialModelProvenance::Scoped});
    }
    if let (Some(provider),Some(id))=(options.default_provider,options.default_model_id)&&let Some(parsed)=resolve_stored_model_reference(provider,id,&runtime.get_models(None))&&parsed.model.as_ref().is_some_and(|m|runtime.get_provider_auth_status(&m.provider).configured){return Ok(InitialModelResult{parsed,fallback_message:None,provenance:InitialModelProvenance::Settings});}
    let available=runtime.get_available(None).await;let model=preferred_available(&available);
    let provenance=if model.as_ref().is_some_and(|m|DEFAULT_MODEL_PER_PROVIDER.contains(&(m.provider.as_str(),m.id.as_str()))){InitialModelProvenance::ProviderDefault}else{InitialModelProvenance::FirstAvailable};
    Ok(InitialModelResult{parsed:ParsedModelResult{model,..Default::default()},fallback_message:None,provenance})
}
pub async fn restore_model_from_session(provider:&str,id:&str,current:Option<Model>,runtime:&crate::model_runtime::ModelRuntime)->InitialModelResult {
    let restored=resolve_stored_model_reference(provider,id,&runtime.get_models(None));
    if let Some(parsed)=&restored&&parsed.model.as_ref().is_some_and(|m|runtime.get_provider_auth_status(&m.provider).configured){return InitialModelResult{parsed:parsed.clone(),fallback_message:None,provenance:InitialModelProvenance::Settings};}
    let reason=if restored.is_none(){"model no longer exists"}else{"no auth configured"};
    let model=if current.is_some(){current}else{preferred_available(&runtime.get_available(None).await)};
    let message=model.as_ref().map(|m|format!("Could not restore model {provider}/{id} ({reason}). Using {}/{}.",m.provider,m.id));
    InitialModelResult{parsed:ParsedModelResult{model,..Default::default()},fallback_message:message,provenance:InitialModelProvenance::FirstAvailable}
}
pub fn resolve_cli_model(provider:Option<&str>,pattern:Option<&str>,thinking:Option<ModelThinkingLevel>,runtime:&crate::model_runtime::ModelRuntime)->ResolveCliModelResult {
    let Some(pattern)=pattern.filter(|p|!p.is_empty()) else{return ResolveCliModelResult::default();};
    let models=runtime.get_models(None);
    if models.is_empty(){return ResolveCliModelResult{error:Some("No models available. Check your installation or add models to models.json.".into()),..Default::default()};}
    let canonical=|id:&str|models.iter().find(|m|m.provider.eq_ignore_ascii_case(id)).map(|m|m.provider.clone());
    let mut selected=provider.and_then(canonical);let mut model_pattern=pattern;
    if provider.is_some()&&selected.is_none(){return ResolveCliModelResult{error:Some(format!("Unknown provider \"{}\". Use --list-models to see available providers/models.",provider.unwrap_or_default())),..Default::default()};}
    let mut inferred=false;
    if selected.is_none()&&let Some((id,suffix))=pattern.split_once('/')&&let Some(id)=canonical(id){selected=Some(id);model_pattern=suffix;inferred=true;}
    if selected.is_none() {
        let exact:Vec<_>=models.iter().filter(|m|m.id.eq_ignore_ascii_case(pattern)||format!("{}/{}",m.provider,m.id).eq_ignore_ascii_case(pattern)).collect();
        if exact.len()==1{return ResolveCliModelResult{parsed:ParsedModelResult{model:Some(exact[0].clone()),..Default::default()},error:None};}
        if exact.len()>1 {
            let authenticated:Vec<_>=exact.iter().filter(|m|runtime.get_provider_auth_status(&m.provider).configured).collect();
            if authenticated.len()==1{return ResolveCliModelResult{parsed:ParsedModelResult{model:Some((*authenticated[0]).clone()),..Default::default()},error:None};}
            let mut ids:Vec<_>=exact.iter().map(|m|format!("{}/{}",m.provider,m.id)).collect();ids.sort();
            let hint=if authenticated.is_empty(){"No matching provider is authenticated."}else{"More than one matching provider is authenticated."};
            return ResolveCliModelResult{error:Some(format!("Model \"{pattern}\" is ambiguous across providers: {}. {hint} Use --provider or provider/model.",ids.join(", "))),..Default::default()};
        }
    }
    if let Some(id)=&selected&&provider.is_some()&&pattern.to_lowercase().starts_with(&format!("{id}/").to_lowercase()){model_pattern=&pattern[id.len()+1..];}
    let candidates:Vec<_>=models.iter().filter(|m|selected.as_ref().is_none_or(|p|*p==m.provider)).cloned().collect();
    let parsed=parse_model_pattern(model_pattern,&candidates,false);
    if parsed.model.is_some(){return ResolveCliModelResult{parsed,error:None};}
    if inferred {
        let parsed=parse_model_pattern(pattern,&models,false);if parsed.model.is_some(){return ResolveCliModelResult{parsed,error:None};}
    }
    if let Some(id)=selected&&let Some(base)=candidates.first() {
        let (model_id,level)=if thinking.is_none(){model_pattern.rsplit_once(':').and_then(|(prefix,suffix)|ModelThinkingLevel::parse(suffix).map(|l|(prefix,Some(l)))).unwrap_or((model_pattern,None))}else{(model_pattern,None)};
        let base=DEFAULT_MODEL_PER_PROVIDER.iter().find(|(p,_)|*p==id).and_then(|(_,default)|candidates.iter().find(|m|m.id==*default)).unwrap_or(base);
        let mut model=base.clone();model.id=model_id.into();model.name=model_id.into();if thinking.or(level).is_some_and(|l|l!=ModelThinkingLevel::Off){model.reasoning=true;}
        return ResolveCliModelResult{parsed:ParsedModelResult{model:Some(model),thinking_level:level,warning:Some(format!("Model \"{model_id}\" not found for provider \"{id}\". Using custom model id.")),..Default::default()},error:None};
    }
    ResolveCliModelResult{error:Some(format!("Model \"{pattern}\" not found. Use --list-models to see available models.")),..Default::default()}
}

#[cfg(test)]
mod tests {
    use super::*;
    fn model(provider:&str,id:&str)->Model {
        let mut model=maho_ai::providers::all::get_builtin_models("openai").remove(0);
        model.provider=provider.into();model.id=id.into();model.name=id.into();model
    }
    #[test]
    fn scope_first_pattern_owns_deduplicated_models(){let models=vec![model("p","sol"),model("q","sol")];let result=resolve_model_scope_from_models(&["p/*:high".into(),"*/sol".into()],&models);assert_eq!(result.scoped_models.len(),2);assert_eq!(result.pattern_resolutions[1].owned_ids,["q/sol"]);assert_eq!(result.scoped_models[0].thinking_level,Some(ModelThinkingLevel::High));}
    #[test]
    fn literal_colon_id_precedes_thinking_decorator(){let models=vec![model("p","sol:high")];let parsed=parse_model_pattern("sol:high",&models,true);assert_eq!(parsed.model.expect("model").id,"sol:high");assert_eq!(parsed.thinking_level,None);}
    #[test]
    fn unknown_scope_is_retained_as_unresolved(){let result=resolve_model_scope_from_models(&["missing".into()],&[model("p","sol")]);assert!(result.pattern_resolutions[0].unresolved);assert_eq!(result.diagnostics[0].code,"no-match");}
}
