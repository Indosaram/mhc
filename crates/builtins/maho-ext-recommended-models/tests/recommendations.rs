use maho_ext_recommended_models::*;
use maho_ai::types::ThinkingLevel;
#[test]
// Upstream defaults: packages/coding-agent/src/core/extensions/builtin/recommended-models/index.ts:25-30.
fn shipped_order(){let index=RECOMMENDED_DEFAULT_MODELS.iter().position(|(id,_)|*id=="gpt-6-astra").expect("astra");assert_eq!(RECOMMENDED_DEFAULT_MODELS[index].1,ThinkingLevel::Xhigh);assert_eq!(RECOMMENDED_DEFAULT_MODELS[index+1],("gpt-6.1-sol",ThinkingLevel::Medium));assert_eq!(RECOMMENDED_DEFAULT_MODELS[index+2],("glm-5.3",ThinkingLevel::Max));}
#[test]
fn aliases(){for id in ["kimi-k3-ultrafast","k3","K3-FAST-256k-unlocked"]{assert_eq!(canonical_model_id(id),"kimi-k3");}assert_eq!(canonical_model_id("gpt-6-astra-fast"),"gpt-6-astra");}
#[test]
// Unknown configured ids use medium: recommended-models/index.ts:51-55.
fn configured_priority(){let ids=vec!["glm-5.2".into(),"custom-fast".into()];let recommendations=recommendations_for(Some(&ids));assert_eq!(recommendations[0].model_id,"glm-5.2");assert_eq!(recommendations[0].thinking_level,ThinkingLevel::Medium);assert_eq!(recommendations[1].thinking_level,ThinkingLevel::Medium);}
#[test]
fn explicit_defaults_respected(){for provenance in ["settings","cli","scoped"]{assert!(!can_auto_switch(Some(provenance)));}}
#[test]
fn implicit_defaults_switch(){for provenance in ["provider-default","first-available"]{assert!(can_auto_switch(Some(provenance)));}}
#[test]
fn empty_override(){assert!(recommendations_for(Some(&[])).is_empty());assert!(!recommendations_for(None).is_empty());}
#[test]
fn lifecycle_registered(){use maho_ext_api::*;let mut api=ExtensionApi::new(LoadedExtension::new("recommended","/tmp".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());RecommendedModels.register(&mut api);assert_eq!(api.get_flag("no-recommended-models"),Some(FlagValue::Boolean(false)));assert_eq!(api.registered.handlers[&EventKind::SessionStart].len(),1);assert_eq!(api.registered.handlers[&EventKind::ModelSelect].len(),1);}
