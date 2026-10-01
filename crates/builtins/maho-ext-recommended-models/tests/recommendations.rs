use maho_ext_recommended_models::*;
use maho_ai::types::ThinkingLevel;
#[test]
fn shipped_order(){let index=RECOMMENDED_DEFAULT_MODELS.iter().position(|(id,_)|*id=="gpt-6-astra").expect("astra");assert_eq!(RECOMMENDED_DEFAULT_MODELS[index].1,ThinkingLevel::High);assert_eq!(RECOMMENDED_DEFAULT_MODELS[index+1],("gpt-6-sol",ThinkingLevel::Medium));assert_eq!(RECOMMENDED_DEFAULT_MODELS[index+2],("gpt-5.6-sol",ThinkingLevel::Medium));}
#[test]
fn aliases(){for id in ["kimi-k3-ultrafast","k3","K3-FAST-256k-unlocked"]{assert_eq!(canonical_model_id(id),"kimi-k3");}assert_eq!(canonical_model_id("gpt-6-astra-fast"),"gpt-6-astra");}
#[test]
fn configured_priority(){let ids=vec!["glm-5.2".into(),"custom-fast".into()];let recommendations=recommendations_for(Some(&ids));assert_eq!(recommendations[0].model_id,"glm-5.2");assert_eq!(recommendations[0].thinking_level,ThinkingLevel::Max);assert_eq!(recommendations[1].thinking_level,ThinkingLevel::Medium);}
#[test]
fn explicit_defaults_respected(){for provenance in ["settings","cli","scoped"]{assert!(!can_auto_switch("tui",Some(provenance)));}}
#[test]
fn implicit_defaults_switch(){for provenance in ["provider-default","first-available"]{assert!(can_auto_switch("tui",Some(provenance)));assert!(!can_auto_switch("app-server",Some(provenance)));}}
#[test]
fn empty_override(){assert!(recommendations_for(Some(&[])).is_empty());assert!(!recommendations_for(None).is_empty());}
