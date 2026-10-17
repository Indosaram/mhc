use maho_ext_recommended_models::*;
use maho_ai::types::ThinkingLevel;
#[test]
fn shipped_order(){assert_eq!(RECOMMENDED_DEFAULT_MODELS,&[
    ("claude-opus-5-5",ThinkingLevel::Medium),
    ("claude-fable-5-1",ThinkingLevel::Xhigh),
    ("kimi-k3",ThinkingLevel::Max),
    ("gpt-6-astra",ThinkingLevel::Xhigh),
    ("gpt-6-sol",ThinkingLevel::Medium),
    ("glm-5.3",ThinkingLevel::Max),
]);}
#[test]
fn aliases(){for id in ["kimi-k3-ultrafast","k3","K3-FAST-256k-unlocked"]{assert_eq!(canonical_model_id(id),"kimi-k3");}assert_eq!(canonical_model_id("gpt-6-astra-fast"),"gpt-6-astra");}
#[test]
fn configured_priority(){let ids=vec!["glm-5.3".into(),"custom-fast".into()];let recommendations=recommendations_for(Some(&ids));assert_eq!(recommendations[0].model_id,"glm-5.3");assert_eq!(recommendations[0].thinking_level,ThinkingLevel::Max);assert_eq!(recommendations[1].thinking_level,ThinkingLevel::Medium);}
#[test]
fn explicit_defaults_respected(){for provenance in ["settings","cli","scoped"]{assert!(!can_auto_switch("tui",Some(provenance)));}}
#[test]
fn implicit_defaults_switch(){for provenance in ["provider-default","first-available"]{assert!(can_auto_switch("tui",Some(provenance)));assert!(!can_auto_switch("app-server",Some(provenance)));}}
#[test]
fn empty_override(){assert!(recommendations_for(Some(&[])).is_empty());assert!(!recommendations_for(None).is_empty());}
#[test]
fn first_available_prefers_astra_over_sol(){
    // Given authenticated models in reverse recommendation order.
    let provider=maho_ai::providers::faux::faux_provider(Default::default());
    let mut sol=provider.get_model(Some("faux-1")).expect("fixture model");
    sol.id="gpt-6-sol".into();
    let mut astra=sol.clone();astra.id="gpt-6-astra".into();
    let available=[sol,astra];
    let recommendations=recommendations_for(None);
    // When startup uses the first available model.
    assert!(can_auto_switch("tui",Some("first-available")));
    let (recommendation,model)=find_available_recommendation(&recommendations,&available,|_|true).expect("recommended model");
    // Then Astra wins with its updated thinking level.
    assert_eq!(model.id,"gpt-6-astra");
    assert_eq!(recommendation.thinking_level,ThinkingLevel::Xhigh);
}
#[test]
fn retired_models_are_not_recommended(){
    let provider=maho_ai::providers::faux::faux_provider(Default::default());
    let mut model=provider.get_model(Some("faux-1")).expect("fixture model");
    let recommendations=recommendations_for(None);
    for id in ["gpt-5.6-sol","glm-5.2"]{
        model.id=id.into();
        assert!(!is_recommended(Some(&model),&recommendations));
    }
}
#[test]
fn lifecycle_registered(){use maho_ext_api::*;let mut api=ExtensionApi::new(LoadedExtension::new("recommended","/tmp".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());RecommendedModels.register(&mut api);assert_eq!(api.get_flag("no-recommended-models"),Some(FlagValue::Boolean(false)));assert_eq!(api.registered.handlers[&EventKind::SessionStart].len(),1);assert_eq!(api.registered.handlers[&EventKind::ModelSelect].len(),1);}
