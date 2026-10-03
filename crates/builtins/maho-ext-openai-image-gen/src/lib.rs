pub mod gate;
pub mod inject;
pub mod externalize;

use maho_ext_api::{Extension, ExtensionApi, ExtensionContext, ExtensionEvent, EventKind, EventResult, ExtensionFailure, Model};
use std::sync::{Arc, Mutex};
use inject::ImageGenMode;

pub const OPENAI_IMAGE_GEN_SECTION: &str = "\n## Image Generation\n\nNative image generation is available in this session.\nGenerate images with the built-in image_generation tool instead of a client-side tool.\n";
struct Arbitration { mode: ImageGenMode, key: String }
fn model_key(model: Option<&Model>) -> String {
    model.map_or_else(String::new,|model|format!("{}|{}|{}|{}",model.provider,model.api,model.base_url,model.id))
}
fn supports_native(model: &Model) -> bool {
    let compat=model.compat.as_ref().map(|compat|serde_json::Value::Object(compat.0.clone()));
    gate::supports_native_image_generation(Some(&gate::NativeImageGenModel{id:&model.id,provider:&model.provider,api:&model.api,base_url:&model.base_url,compat:compat.as_ref()}))
}
async fn refresh(state: &Mutex<Arbitration>, model: Option<&Model>, ctx: &ExtensionContext) -> Result<ImageGenMode, ExtensionFailure> {
    let key = model_key(model);
    let mode = if model.is_none() { ImageGenMode::Unavailable }
        else if gate::is_open_ai_image_gen_enabled() && model.is_some_and(supports_native) { ImageGenMode::Native }
        else { match maho_ext_imagegen::auth::resolve_context_image_gen_auth(ctx).await? { maho_ext_imagegen::auth::ImageGenAuthResolution::Configured { .. } => ImageGenMode::Client, maho_ext_imagegen::auth::ImageGenAuthResolution::None { .. } => ImageGenMode::Unavailable } };
    *state.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Arbitration { mode, key };
    maho_ext_imagegen::state::set_native_bypass(mode == ImageGenMode::Native);
    Ok(mode)
}
async fn ensure_fresh(state: &Mutex<Arbitration>, model: Option<&Model>, ctx: &ExtensionContext) -> Result<ImageGenMode, ExtensionFailure> {
    let key = model_key(model);
    let cached = { let state = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner); (state.key == key).then_some(state.mode) };
    match cached { Some(mode) => Ok(mode), None => refresh(state, model, ctx).await }
}
pub struct OpenAiImageGen;
impl Extension for OpenAiImageGen {
    fn register(&self, api: &mut ExtensionApi) {
        let state = Arc::new(Mutex::new(Arbitration { mode: ImageGenMode::Unavailable, key: String::new() }));
        for kind in [EventKind::SessionStart, EventKind::ModelSelect, EventKind::BeforeProviderRequest, EventKind::BeforeAgentStart] {
            let state = state.clone();
            api.on(kind, Arc::new(move |event, ctx| {
                let state = state.clone();
                Box::pin(async move {
                    let model = match event { ExtensionEvent::ModelSelect(event) => Some(&event.model), ExtensionEvent::BeforeProviderRequest {model,..} => model.as_ref().or(ctx.model.as_ref()), _ => ctx.model.as_ref() };
                    let mode = if matches!(kind,EventKind::SessionStart|EventKind::ModelSelect) { refresh(&state,model,ctx).await? } else { ensure_fresh(&state,model,ctx).await? };
                    Ok(match event {
                        ExtensionEvent::BeforeProviderRequest {payload,..} => EventResult::ProviderPayload(inject::apply_image_generation_tools(payload,mode)),
                        ExtensionEvent::BeforeAgentStart(event) if mode == ImageGenMode::Native => EventResult::BeforeAgentStart(maho_ext_api::BeforeAgentStartEventResult {message:None,system_prompt:Some(format!("{}\n{}",event.system_prompt,OPENAI_IMAGE_GEN_SECTION))}),
                        _ => EventResult::None,
                    })
                })
            }));
        }
        api.on(EventKind::MessageEnd,Arc::new(|event,ctx|Box::pin(async move {
            if let ExtensionEvent::MessageEnd {message} = event
                && let Some(assistant) = message.as_assistant()
                && let Some(replaced) = externalize::externalize_native_images(assistant,&ctx.cwd) {
                return Ok(EventResult::MessageEnd {message:Some(maho_agent::types::AgentMessage::Llm(maho_ai::types::Message::Assistant(Box::new(replaced))))});
            }
            Ok(EventResult::None)
        })));
        api.on(EventKind::SessionShutdown,Arc::new(|_,_|Box::pin(async { maho_ext_imagegen::state::set_native_bypass(false); Ok(EventResult::None) })));
    }
}
