use std::sync::{Arc,Mutex};
use maho_ext_api::*;
use crate::{detection::*,directive::*,notice::*,architect_gate::has_active_architect_category};
#[derive(Default)]
struct State { refusal_pending:bool,active:Option<(String,String)> }
pub struct FallbackArchitectComponent;
impl Extension for FallbackArchitectComponent {
    fn register(&self,api:&mut ExtensionApi) {
        let state=Arc::new(Mutex::new(State::default())); let runtime=api.runtime.clone();
        api.register_message_renderer(FALLBACK_ARCHITECT_NOTICE_TYPE,Arc::new(|message,_,_| {
            let details=message.details.as_ref().and_then(|v|Some((v["from"].as_str()?,v["to"].as_str()?)));
            Some(Box::new(maho_tui::components::text::Text::with_padding(notice_lines(details).join("\n"),0,0)))
        }));
        let message_state=Arc::clone(&state);
        api.on(EventKind::MessageEnd,Arc::new(move |event,_| { let state=Arc::clone(&message_state); Box::pin(async move {
            if let ExtensionEvent::MessageEnd{message}=event {
                let value=serde_json::to_value(message).map_err(|e|ExtensionFailure::new(e.to_string()))?;
                if value["role"]=="assistant" { state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).refusal_pending=is_refusal_like_message(&value); }
            }
            Ok(EventResult::None)
        }) }));
        let model_state=Arc::clone(&state); let model_runtime=runtime.clone();
        api.on(EventKind::ModelSelect,Arc::new(move |event,ctx| { let state=Arc::clone(&model_state); let runtime=model_runtime.clone(); Box::pin(async move {
            let ExtensionEvent::ModelSelect(event)=event else { return Ok(EventResult::None); };
            if runtime.get_flag("omo-senpi-fallback-architect-disabled")==Some(FlagValue::Boolean(true)) { return Ok(EventResult::None); }
            let mut state=state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if event.model.id==FABLE_FIVE_MODEL_ID || event.source==ModelSelectSource::FallbackRevert { state.active=None;state.refusal_pending=false;return Ok(EventResult::None); }
            if event.source!=ModelSelectSource::Fallback || event.previous_model.as_ref().is_none_or(|m|m.id!=FABLE_FIVE_MODEL_ID) || !state.refusal_pending { return Ok(EventResult::None); }
            if !has_active_architect_category(&ctx.cwd,Some(ctx.model_registry.as_ref())) { state.refusal_pending=false;return Ok(EventResult::None); }
            let Some(previous)=&event.previous_model else { return Ok(EventResult::None); };
            let from=format!("{}/{}",previous.provider,previous.id); let to=format!("{}/{}",event.model.provider,event.model.id);
            let api=ExtensionApi::new(LoadedExtension::new("fallback-architect",ctx.cwd.clone(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),runtime);
            api.send_message(CustomMessage{custom_type:FALLBACK_ARCHITECT_DIRECTIVE_TYPE.into(),content:vec![ToolContent::text(build_fallback_architect_directive(&from,&to))],display:false,details:None},SendMessageOptions::default())?;
            api.send_message(CustomMessage{custom_type:FALLBACK_ARCHITECT_NOTICE_TYPE.into(),content:vec![ToolContent::text(build_fallback_architect_notice(&from,&to))],display:true,details:Some(serde_json::json!({"from":from,"to":to}))},SendMessageOptions::default())?;
            state.active=Some((from,to));state.refusal_pending=false;
            Ok(EventResult::None)
        }) }));
        api.on(EventKind::Input,Arc::new(move |event,ctx| { let state=Arc::clone(&state);let runtime=runtime.clone();Box::pin(async move {
            let ExtensionEvent::Input(input)=event else { return Ok(EventResult::None); };
            if input.source!=InputSource::Extension && runtime.get_flag("omo-senpi-fallback-architect-disabled")!=Some(FlagValue::Boolean(true)) && let Some((from,_))=&state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).active {
                let api=ExtensionApi::new(LoadedExtension::new("fallback-architect",ctx.cwd.clone(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),runtime);
                api.send_message(CustomMessage{custom_type:FALLBACK_ARCHITECT_REMINDER_TYPE.into(),content:vec![ToolContent::text(build_fallback_architect_reminder(from))],display:false,details:None},SendMessageOptions::default())?;
            }
            Ok(EventResult::Input(InputEventResult::Continue))
        }) }));
    }
}
