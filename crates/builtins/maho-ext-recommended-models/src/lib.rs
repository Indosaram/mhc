use maho_ai::{model::Model,types::ThinkingLevel};

pub const RECOMMENDED_DEFAULT_MODELS: &[(&str,ThinkingLevel)] = &[
    ("kimi-k3",ThinkingLevel::Max),("gpt-6-astra",ThinkingLevel::High),
    ("gpt-6-sol",ThinkingLevel::Medium),("gpt-5.6-sol",ThinkingLevel::Medium),
    ("claude-fable-5-1",ThinkingLevel::High),("claude-opus-5-5",ThinkingLevel::Max),
    ("glm-5.2",ThinkingLevel::Max),
];
pub fn canonical_model_id(id:&str)->String{
    let mut id=id.to_lowercase();
    while let Some(suffix)=["-ultrafast","-unlocked","-256k","-fast"].iter().find(|suffix|id.ends_with(**suffix)){
        id.truncate(id.len()-suffix.len());
    }
    if id=="k3"{"kimi-k3".into()}else{id}
}
#[derive(Clone,Debug,PartialEq,Eq)]
pub struct Recommendation{pub model_id:String,pub thinking_level:ThinkingLevel}
pub fn recommendations_for(configured:Option<&[String]>)->Vec<Recommendation>{
    configured.map_or_else(||RECOMMENDED_DEFAULT_MODELS.iter().map(|(id,level)|Recommendation{model_id:(*id).into(),thinking_level:*level}).collect(),|ids|{
        ids.iter().map(|id|canonical_model_id(id)).filter(|id|!id.is_empty()).map(|model_id|{
            let thinking_level=RECOMMENDED_DEFAULT_MODELS.iter().find(|(id,_)|*id==model_id).map_or(ThinkingLevel::Medium,|(_,level)|*level);
            Recommendation{model_id,thinking_level}
        }).collect()
    })
}
pub fn is_recommended(model:Option<&Model>,recommendations:&[Recommendation])->bool{
    model.is_some_and(|model|recommendations.iter().any(|recommendation|recommendation.model_id==canonical_model_id(&model.id)))
}
pub fn find_available_recommendation<'a>(recommendations:&'a [Recommendation],available:&'a [Model],has_auth:impl Fn(&Model)->bool)->Option<(&'a Recommendation,&'a Model)>{
    for recommendation in recommendations{
        if let Some(model)=available.iter().find(|model|has_auth(model)&&canonical_model_id(&model.id)==recommendation.model_id){return Some((recommendation,model));}
    }
    None
}
pub fn can_auto_switch(mode:&str,provenance:Option<&str>)->bool{mode!="app-server"&&matches!(provenance,Some("provider-default"|"first-available"))}

use maho_ext_api::{Extension,ExtensionApi,ExtensionContext,ExtensionEvent,EventKind,EventResult,FlagType,FlagValue,ExtensionMode,ModelSelectSource,NotificationType,ExtensionFailure};
use std::sync::{Arc,Mutex};
struct State{initialized:bool,disabled:bool,disarmed:bool,automatic:bool,warning_shown:bool,can_switch:bool,recommendations:Vec<Recommendation>}
fn warn(state:&mut State,ctx:&ExtensionContext){
    if state.warning_shown||find_available_recommendation(&state.recommendations,&ctx.model_registry.get_available(),|model|ctx.model_registry.has_configured_auth(model)).is_some(){return;}
    state.warning_shown=true;ctx.ui.notify(&format!("Non-recommended model '{}': odd behavior is the default state; a working session is the anomaly.",ctx.model.as_ref().map_or("<none>",|model|model.id.as_str())),NotificationType::Warning);
}
pub struct RecommendedModels;
impl Extension for RecommendedModels{
    fn register(&self,api:&mut ExtensionApi){
        api.register_flag("no-recommended-models",FlagType::Boolean{default:Some(false)},Some("Disable recommended model selection for this run.".into()));
        let state=Arc::new(Mutex::new(State{initialized:false,disabled:false,disarmed:false,automatic:false,warning_shown:false,can_switch:false,recommendations:recommendations_for(None)}));
        let start=Arc::clone(&state);let runtime=api.runtime.clone();
        api.on(EventKind::SessionStart,Arc::new(move|event,ctx|{
            let state=Arc::clone(&start);let runtime=runtime.clone();
            Box::pin(async move{
                let ExtensionEvent::SessionStart(event)=event else{return Ok(EventResult::None)};
                let target={
                    let mut guard=state.lock().expect("recommendation state lock");if guard.initialized{return Ok(EventResult::None);}guard.initialized=true;
                    let home=std::env::var("HOME").map_err(|error|ExtensionFailure::new(error.to_string()))?;
                    let settings=maho_core::settings_manager::SettingsManager::create(&ctx.cwd.to_string_lossy(),&ctx.agent_dir.to_string_lossy(),&home,ctx.is_project_trusted());
                    guard.disabled=runtime.get_flag("no-recommended-models")==Some(FlagValue::Boolean(true))||settings.get().get("warnings").and_then(|value|value.get("offRecommendedModel")).and_then(|value|value.as_bool())==Some(true);
                    let configured=settings.get().get("recommendedModels").and_then(|value|value.as_array()).map(|ids|ids.iter().filter_map(|id|id.as_str().map(str::to_owned)).collect::<Vec<_>>());
                    guard.recommendations=recommendations_for(configured.as_deref());
                    guard.can_switch=ctx.mode!=ExtensionMode::AppServer&&matches!(event.initial_model_provenance.as_deref(),Some("provider-default"|"first-available"));
                    if guard.disabled||!guard.can_switch||is_recommended(ctx.model.as_ref(),&guard.recommendations){return Ok(EventResult::None);}
                    let available=ctx.model_registry.get_available();
                    let target=find_available_recommendation(&guard.recommendations,&available,|model|ctx.model_registry.has_configured_auth(model)).map(|(recommendation,model)|(recommendation.clone(),model.clone()));
                    if target.is_none(){warn(&mut guard,ctx);return Ok(EventResult::None);}guard.automatic=true;target.expect("target checked")
                };
                let result=async{
                    let actions=runtime.session_actions()?;let persist=ctx.mode==ExtensionMode::Tui;
                    let switched=if persist{actions.set_model(target.1.clone()).await?}else{actions.set_session_model(target.1.clone()).await?};
                    if !switched{warn(&mut state.lock().expect("recommendation state lock"),ctx);return Ok(EventResult::None);}
                    if persist{actions.set_thinking_level(target.0.thinking_level)?;}else{actions.set_session_thinking_level(target.0.thinking_level)?;}
                    ctx.ui.notify(&format!("Switched to recommended model '{}'.",target.1.id),NotificationType::Info);Ok(EventResult::None)
                }.await;
                state.lock().expect("recommendation state lock").automatic=false;result
            })
        }));
        api.on(EventKind::ModelSelect,Arc::new(move|event,ctx|{
            let state=Arc::clone(&state);
            Box::pin(async move{
                let ExtensionEvent::ModelSelect(event)=event else{return Ok(EventResult::None)};let mut guard=state.lock().expect("recommendation state lock");
                if !guard.initialized||guard.disabled||guard.disarmed||guard.automatic||!guard.can_switch{return Ok(EventResult::None);}
                if matches!(event.source,ModelSelectSource::Set|ModelSelectSource::Cycle){guard.disarmed=true;return Ok(EventResult::None);}
                if !is_recommended(Some(&event.model),&guard.recommendations){warn(&mut guard,ctx);}Ok(EventResult::None)
            })
        }));
    }
}

