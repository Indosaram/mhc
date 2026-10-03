use maho_codemode::completion::handler::{CompletionError, CompletionRequest, CompletionTier};
use maho_ext_api::ExtensionContext;

struct CompletionCancellation(maho_ai::utils::abort::AbortController);
impl Drop for CompletionCancellation {
    fn drop(&mut self) { self.0.abort(None); }
}

pub async fn complete(request: CompletionRequest, context: ExtensionContext) -> Result<serde_json::Value, CompletionError> {
    if context.signal.as_ref().is_some_and(|signal| signal.is_aborted()) {
        return Err(CompletionError("completion() request aborted.".into()));
    }
    if let Some(error) = context.session_manager.extension_context_actions().map(|actions| actions.assert_active()).transpose().err() {
        return Err(CompletionError(error.message));
    }
    let request = maho_codemode::completion::handler::normalize_request(request);
    let tier = maho_codemode::completion::handler::resolve_completion_tier(request.model.as_deref())?;
    let model = match tier {
        CompletionTier::Default => context.model.clone(),
        CompletionTier::Smol | CompletionTier::Slow => {
            let mut selected: Option<maho_ai::types::Model> = None;
            for model in context.model_registry.get_available() {
                let cost = model.cost.input + model.cost.output;
                if selected.as_ref().is_none_or(|current| match tier {
                    CompletionTier::Smol => cost < current.cost.input + current.cost.output,
                    CompletionTier::Slow => cost > current.cost.input + current.cost.output,
                    CompletionTier::Default => false,
                }) { selected = Some(model); }
            }
            selected
        }
    }.ok_or_else(|| CompletionError("completion() has no model/credentials".into()))?;
    let resolution = context.model_registry.get_api_key_and_headers(&model);
    let auth = if let Some(signal) = &context.signal {
        tokio::select! {
            biased;
            () = signal.cancelled() => return Err(CompletionError("completion() request aborted.".into())),
            result = resolution => result,
        }
    } else { resolution.await }
        .map_err(|error| CompletionError(format!("completion() has no model/credentials: {}", error.message)))?;
    let api_key = auth.auth.api_key.ok_or_else(|| CompletionError("completion() has no model/credentials".into()))?;
    let mut request_model = model.clone();
    if let Some(id) = auth.upstream_model_id { request_model.id = id; }
    if let Some(url) = auth.auth.base_url { request_model.base_url = url; }
    request_model.service_tier = auth.service_tier.or(request_model.service_tier);
    let prompt = request.schema.as_ref().map_or_else(|| request.prompt.clone(), |schema|
        maho_codemode::bridges::schema_injection::inject_schema_instruction(&request.prompt, schema));
    let controller = maho_ai::utils::abort::AbortController::new();
    let _cancellation = CompletionCancellation(controller.clone());
    if context.signal.as_ref().is_some_and(|signal| signal.is_aborted()) {
        return Err(CompletionError("completion() request aborted.".into()));
    }
    let stream = context.model_registry.stream_simple(&request_model, &maho_ai::types::Context {
        system_prompt: Some(request.system.unwrap_or_else(|| "You are a helpful assistant.".into())),
        messages: vec![maho_ai::types::Message::User(maho_ai::types::UserMessage {
            content: maho_ai::types::UserContent::Blocks(vec![maho_ai::types::ContentBlock::text(prompt)]),
            timestamp: maho_ai::utils::diagnostics::now_ms(),
        })], tools: None,
    }, Some(maho_ai::types::SimpleStreamOptions { stream: maho_ai::types::StreamOptions {
        request: maho_ai::types::ProviderRequestOptions { api_key: Some(api_key), headers: auth.auth.headers,
            env: auth.env, signal: Some(controller.signal()), ..Default::default() },
        extra_body: auth.extra_body, ..Default::default()
    }, service_tier: auth.service_tier, ..Default::default() })).map_err(|error| CompletionError(error.message))?;
    let result = if let Some(signal) = context.signal {
        tokio::select! {
            biased;
            () = signal.cancelled() => { controller.abort(None); return Err(CompletionError("completion() request aborted.".into())); }
            result = stream.result() => result,
        }
    } else { stream.result().await };
    let message = result.map_err(|error| CompletionError(error.to_string()))?;
    maho_codemode::completion::handler::format_completion(&message, &model.provider, &model.id, request.schema.is_some())
}

pub struct ImageSdk;
impl maho_codemode::tool::image_resize::EvalImageSdk for ImageSdk {
    fn resize_image<'a>(&'a self, bytes: Vec<u8>, mime_type: &'a str, max_bytes: Option<usize>) -> maho_codemode::tool::image_resize::ImageFuture<'a, Option<maho_codemode::tool::image_resize::ResizedImage>> {
        Box::pin(async move {
            let mut options = crate::utils::image_resize::ImageResizeOptions::default();
            if let Some(max_bytes) = max_bytes { options.max_bytes = max_bytes as f64; }
            let result = crate::utils::image_process::process_image(&bytes, mime_type,
                crate::utils::image_process::ProcessImageOptions { resize_options: Some(options), ..Default::default() }).await?;
            Ok(Some(maho_codemode::tool::image_resize::ResizedImage { data: result.data,
                mime_type: result.mime_type, dimension_note: result.hints.join("\n") }))
        })
    }
    fn convert_to_png<'a>(&'a self, data: &'a str, mime_type: &'a str) -> maho_codemode::tool::image_resize::ImageFuture<'a, Option<maho_codemode::tool::image_resize::EvalImageContent>> {
        Box::pin(async move { Ok(crate::utils::image_convert::convert_to_png(data, mime_type).map(|image|
            maho_codemode::tool::image_resize::EvalImageContent { data: image.data, mime_type: image.mime_type })) })
    }
}
