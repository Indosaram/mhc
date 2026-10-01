use maho_ai::types::Model;
use serde_json::Value;
use super::{chains::{base_selector,parse_fallback_selector},expansion::{matches_family,parse_bare_selector}};

pub fn validate_fallback_chains(chains:Option<&Value>,models:&[Model]) -> Vec<String> {
    let Some(chains)=chains else {return Vec::new();};
    let Some(chains)=chains.as_object() else {let kind=match chains {Value::Null=>"null",Value::Array(_)=>"an array",Value::Bool(_)=>"a boolean",Value::Number(_)=>"a number",Value::String(_)=>"a string",Value::Object(_)=>"an object"};return vec![format!("Fallback chains must be a plain object, but got {kind}.")];};
    let mut warnings=Vec::new();
    for (key,entries) in chains {
        let provider=key.contains('/');let wildcard=key.contains('*');
        let bare=parse_bare_selector(key);
        if !provider&&!wildcard&&!bare.as_ref().is_some_and(|b|models.iter().any(|m|matches_family(m,&b.family))) {warnings.push(format!("Fallback chain key \"{key}\" must use a provider/model selector; roles are unsupported."));}
        if wildcard&&key!="*" {warnings.push(format!("Fallback chain key \"{key}\" cannot contain wildcards."));}
        let parsed=if provider&&!wildcard {parse_fallback_selector(key,models)} else {None};
        if provider&&!wildcard&&parsed.is_none(){warnings.push(format!("Fallback chain key \"{key}\" is not a valid or known model selector."));}
        if let Some(selector)=&parsed{validate_thinking(selector,models,&format!("Fallback chain key \"{key}\""),&mut warnings);}
        let Some(entries)=entries.as_array().filter(|v|v.iter().all(Value::is_string)) else {warnings.push(format!("Fallback chain \"{key}\" entries must be an array of strings."));continue;};
        for entry in entries.iter().filter_map(Value::as_str) {
            if let Some(bare)=parse_bare_selector(entry) {if !models.iter().any(|m|matches_family(m,&bare.family)){warnings.push(format!("Fallback chain entry \"{entry}\" for \"{key}\" is not a valid or known model selector."));}continue;}
            let Some(selector)=parse_fallback_selector(entry,models) else {warnings.push(format!("Fallback chain entry \"{entry}\" for \"{key}\" is not a valid or known model selector."));continue;};
            if parsed.as_ref().is_some_and(|p|base_selector(p)==base_selector(&selector)){warnings.push(format!("Fallback chain entry \"{entry}\" for \"{key}\" cannot reference the same model."));}
            validate_thinking(&selector,models,&format!("Fallback chain entry \"{entry}\""),&mut warnings);
        }
    }
    warnings
}

fn validate_thinking(selector:&super::chains::FallbackSelector,models:&[Model],subject:&str,warnings:&mut Vec<String>){
    if let Some(level)=selector.thinking_level&&let Some(model)=models.iter().find(|m|m.provider==selector.provider&&m.id==selector.id)&&!maho_ai::models::get_supported_thinking_levels(model).contains(&level){warnings.push(format!("{subject} uses thinking level \"{}\", which is unsupported by {}/{}.",level.as_str(),selector.provider,selector.id));}
}
