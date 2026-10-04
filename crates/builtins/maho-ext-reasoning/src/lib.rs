use maho_ai::types::{ThinkingLevel,ModelThinkingLevel};
use maho_core::thinking_levels::ReasoningCapability;
pub const EFFORT_LEVELS:[ThinkingLevel;6]=[ThinkingLevel::Minimal,ThinkingLevel::Low,ThinkingLevel::Medium,ThinkingLevel::High,ThinkingLevel::Xhigh,ThinkingLevel::Max];
pub fn parse_single_argument(args:&str)->Option<&str>{let args=args.trim();if args.is_empty(){return Some("");}let mut tokens=args.split_whitespace();let first=tokens.next();if tokens.next().is_some(){None}else{first}}
pub fn clamp_to_non_off(level:ModelThinkingLevel,capability:&ReasoningCapability)->Option<ThinkingLevel>{
    let levels=ModelThinkingLevel::ALL;
    let index=levels.iter().position(|candidate|*candidate==level).expect("known level");
    let supported=|candidate:ModelThinkingLevel|capability.non_off_levels.iter().copied().find(|level|ModelThinkingLevel::from(*level)==candidate);
    for candidate in levels.iter().skip(index){if let Some(level)=supported(*candidate){return Some(level);}}
    for candidate in levels[..index].iter().rev(){if let Some(level)=supported(*candidate){return Some(level);}}
    capability.non_off_levels.first().copied()
}
pub fn preferred_on_level(last_on:Option<ModelThinkingLevel>,remembered:Option<ModelThinkingLevel>,global:Option<ModelThinkingLevel>,capability:&ReasoningCapability)->Option<ThinkingLevel>{
    let non_off=|level:Option<ModelThinkingLevel>|level.filter(|level|*level!=ModelThinkingLevel::Off);
    clamp_to_non_off(non_off(last_on).or_else(||non_off(remembered)).or_else(||non_off(global)).unwrap_or(ModelThinkingLevel::Medium),capability)
}
pub fn completions<'a>(values:&'a [&str],prefix:&str)->Option<Vec<&'a str>>{let values:Vec<_>=values.iter().copied().filter(|value|value.starts_with(prefix.trim())).collect();(!values.is_empty()).then_some(values)}

use maho_ext_api::*;
use maho_core::thinking_levels::{classify_reasoning_capability,ReasoningCapabilityKind};
use std::sync::{Arc,Mutex};
#[derive(Default)]
pub struct ThinkingPreferences{pub last_on:Option<ModelThinkingLevel>,pub remembered:Option<ModelThinkingLevel>,pub global:Option<ModelThinkingLevel>}
/// Host operations must use the effective session level and durable model preferences.
pub trait ReasoningHost:Send+Sync{
    fn level(&self,ctx:&ExtensionContext)->Result<ModelThinkingLevel,ExtensionFailure>;
    fn set_level(&self,ctx:&ExtensionContext,level:ModelThinkingLevel)->Result<(),ExtensionFailure>;
    fn preferences(&self,ctx:&ExtensionContext,model:&Model)->Result<ThinkingPreferences,ExtensionFailure>;
    fn remember_last_on<'a>(&'a self,ctx:&'a ExtensionContext,model:&'a Model,level:ModelThinkingLevel)->ExtensionFuture<'a,()>;
    fn restore_on<'a>(&'a self,ctx:&'a ExtensionContext,model:&'a Model,level:ThinkingLevel)->ExtensionFuture<'a,()>;
}
pub struct Reasoning{pub host:Arc<dyn ReasoningHost>}
fn reasoning_status(level:ModelThinkingLevel)->String{if level==ModelThinkingLevel::Off{"Reasoning: off.".into()}else{format!("Reasoning: on ({}).",level.as_str())}}
fn efforts_status(level:ModelThinkingLevel,capability:&ReasoningCapability)->String{format!("Reasoning effort: {}. Available: {}.",level.as_str(),capability.non_off_levels.iter().map(|level|level.as_str()).collect::<Vec<_>>().join(", "))}
fn complete(values:Vec<&str>,prefix:&str)->Option<Vec<maho_tui::autocomplete::AutocompleteItem>>{
    let values=values.into_iter().filter(|value|value.starts_with(prefix.trim())).map(|value|maho_tui::autocomplete::AutocompleteItem{value:value.into(),label:value.into(),description:None}).collect::<Vec<_>>();
    (!values.is_empty()).then_some(values)
}
impl Extension for Reasoning{
    fn register(&self,api:&mut ExtensionApi){
        let current=Arc::new(Mutex::new(None::<Model>));
        for kind in [EventKind::SessionStart,EventKind::ModelSelect]{let current=current.clone();api.on(kind,Arc::new(move|event,ctx|{let current=current.clone();Box::pin(async move{*current.lock().unwrap_or_else(std::sync::PoisonError::into_inner)=match event{ExtensionEvent::ModelSelect(event)=>Some(event.model.clone()),_=>ctx.model.clone()};Ok(EventResult::None)})}));}
        for name in ["reasoning","efforts"]{
            let host=self.host.clone();let observed=current.clone();let completion_model=current.clone();
            api.register_command_with_completions(name,Some(if name=="reasoning"{"Show or toggle reasoning for the current model"}else{"Show or set the reasoning effort for the current model"}.into()),Some(if name=="reasoning"{"[on|off]"}else{"[minimal|low|medium|high|xhigh|max]"}.into()),Arc::new(move|args,ctx|{let host=host.clone();let observed=observed.clone();Box::pin(async move{
                let Some(model)=ctx.model.as_ref()else{ctx.ui.notify("No model is active.",NotificationType::Error);return Ok(())};
                *observed.lock().unwrap_or_else(std::sync::PoisonError::into_inner)=Some(model.clone());
                let argument=parse_single_argument(args);
                let valid=argument.is_some_and(|argument|argument.is_empty()||if name=="reasoning"{matches!(argument,"on"|"off")}else{ModelThinkingLevel::parse(argument).is_some()});
                if !valid{ctx.ui.notify(if name=="reasoning"{"Usage: /reasoning [on|off]"}else{"Usage: /efforts [minimal|low|medium|high|xhigh|max]"},NotificationType::Error);return Ok(());}
                let argument=argument.expect("validated argument");let capability=classify_reasoning_capability(model);let key=format!("{}/{}",model.provider,model.id);
                let level=host.level(ctx)?;
                if name=="reasoning"{
                    if argument.is_empty(){ctx.ui.notify(&reasoning_status(level),NotificationType::Info);return Ok(());}
                    if argument=="off"{
                        match capability.kind{
                            ReasoningCapabilityKind::None=>ctx.ui.notify(&reasoning_status(ModelThinkingLevel::Off),NotificationType::Info),
                            ReasoningCapabilityKind::AlwaysOn=>ctx.ui.notify(&format!("Reasoning cannot be disabled for {key}."),NotificationType::Error),
                            _=>{if level!=ModelThinkingLevel::Off{host.remember_last_on(ctx,model,level).await?;}host.set_level(ctx,ModelThinkingLevel::Off)?;ctx.ui.notify(&reasoning_status(host.level(ctx)?),NotificationType::Info);}
                        }
                    }else if capability.kind==ReasoningCapabilityKind::None{ctx.ui.notify(&format!("Model {key} does not support reasoning."),NotificationType::Error);}
                    else if level!=ModelThinkingLevel::Off{host.set_level(ctx,level)?;ctx.ui.notify(&reasoning_status(host.level(ctx)?),NotificationType::Info);}
                    else{
                        let preferences=host.preferences(ctx,model)?;
                        if let Some(target)=preferred_on_level(preferences.last_on,preferences.remembered,preferences.global,&capability){host.restore_on(ctx,model,target).await?;ctx.ui.notify(&reasoning_status(host.level(ctx)?),NotificationType::Info);}else{ctx.ui.notify(&format!("Model {key} does not support reasoning."),NotificationType::Error);}
                    }
                }else{
                    match capability.kind{
                        ReasoningCapabilityKind::None=>ctx.ui.notify(&format!("Model {key} does not support reasoning."),NotificationType::Error),
                        ReasoningCapabilityKind::OnOff=>ctx.ui.notify(&format!("Reasoning effort is not configurable for {key}; this model supports on/off only. Use /reasoning on or /reasoning off."),NotificationType::Error),
                        _=>{
                            if argument.is_empty(){ctx.ui.notify(&efforts_status(level,&capability),NotificationType::Info);}
                            else if let Some(target)=capability.non_off_levels.iter().find(|level|level.as_str()==argument){host.set_level(ctx,(*target).into())?;ctx.ui.notify(&efforts_status(host.level(ctx)?,&capability),NotificationType::Info);}
                            else{ctx.ui.notify(&format!("Reasoning effort \"{argument}\" is not supported by {key}. Available: {}.",capability.non_off_levels.iter().map(|level|level.as_str()).collect::<Vec<_>>().join(", ")),NotificationType::Error);}
                        }
                    }
                }
                Ok(())
            })}),Arc::new(move|prefix|{let values=if name=="reasoning"{vec!["on","off"]}else{completion_model.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref().map(classify_reasoning_capability).filter(|capability|capability.kind==ReasoningCapabilityKind::Graded).map_or_else(Vec::new,|capability|capability.non_off_levels.into_iter().map(|level|level.as_str()).collect())};Box::pin(async move{Ok(complete(values,prefix))})}));
        }
    }
}

