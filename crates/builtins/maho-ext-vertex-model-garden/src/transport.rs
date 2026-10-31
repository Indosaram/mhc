use maho_ai::{
    api::{
        google_shared::google_vertex_adc::{VertexAdcAccess, get_vertex_adc_access},
        lazy::lazy_stream,
    },
    model::Model,
    types::{AssistantMessageEventStream, Context, ProviderEnv, SimpleStreamOptions},
};
use std::{future::Future, pin::Pin, sync::Arc};

pub const VERTEX_MODEL_GARDEN_LOCATION: &str = "global";

pub type ResolveVertexAccess = Arc<
    dyn Fn(
            Option<ProviderEnv>,
        ) -> Pin<Box<dyn Future<Output = Result<VertexAdcAccess, String>> + Send>>
        + Send
        + Sync,
>;

/// The project comes from its own environment (`options.env`, then the ADC quota project, then discovery),
/// never from the API-key slot: a stored secret must not end up in the request URL, and the project may
/// legitimately come from ADC alone.
pub fn default_resolve_vertex_access() -> ResolveVertexAccess {
    Arc::new(|env| {
        Box::pin(async move {
            get_vertex_adc_access(&reqwest::Client::new(), None, env.as_ref())
                .await
                .map_err(|error| error.to_string())
        })
    })
}

pub fn vertex_host(location: &str) -> String {
    if location == "global" {
        "aiplatform.googleapis.com".into()
    } else {
        format!("{location}-aiplatform.googleapis.com")
    }
}

fn encode_uri_component(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes())
        .collect::<String>()
        .replace('+', "%20")
}

pub fn build_vertex_openapi_base_url(project: &str, location: &str) -> String {
    format!(
        "https://{}/v1/projects/{}/locations/{location}/endpoints/openapi",
        vertex_host(location),
        encode_uri_component(project)
    )
}

pub fn stream_vertex_model_garden(
    model: &Model,
    context: &Context,
    options: Option<SimpleStreamOptions>,
    resolve_access: ResolveVertexAccess,
) -> AssistantMessageEventStream {
    let (target_model, context) = (model.clone(), context.clone());
    lazy_stream(model, async move {
        let mut options = options.unwrap_or_default();
        let access = resolve_access(options.stream.request.env.clone()).await?;
        let mut target = target_model;
        target.base_url =
            build_vertex_openapi_base_url(&access.project, VERTEX_MODEL_GARDEN_LOCATION);
        options.stream.request.api_key = Some(access.access_token);
        options
            .stream
            .request
            .headers
            .get_or_insert_with(Default::default)
            .insert("x-goog-user-project".into(), Some(access.quota_project));
        let provider = maho_ai::api_registry::get_api_provider("openai-completions")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "API provider openai-completions is not registered".to_owned())?;
        Ok(provider.stream_simple(&target, &context, Some(options)))
    })
    .into_stream()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_location_uses_the_bare_host_and_projects_are_encoded() {
        assert_eq!(vertex_host("global"), "aiplatform.googleapis.com");
        assert_eq!(
            vertex_host("us-east5"),
            "us-east5-aiplatform.googleapis.com"
        );
        assert_eq!(
            build_vertex_openapi_base_url("my proj", "global"),
            "https://aiplatform.googleapis.com/v1/projects/my%20proj/locations/global/endpoints/openapi"
        );
    }
}
