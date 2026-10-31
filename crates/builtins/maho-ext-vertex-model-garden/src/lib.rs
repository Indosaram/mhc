//! Port of senpi core/extensions/builtin/vertex-model-garden (index.ts, auth.ts, models.ts, transport.ts).
pub mod auth;
pub mod models;
pub mod transport;

use auth::{StoredProviderEnvReader, VertexModelGardenAmbientAuth};
use maho_ext_api::{Extension, ExtensionApi, ProviderConfig};
use std::sync::Arc;
use transport::{ResolveVertexAccess, default_resolve_vertex_access, stream_vertex_model_garden};

pub const VERTEX_MODEL_GARDEN_PROVIDER_ID: &str = "vertex-garden";

// The request URL is built per call from the ADC-resolved project; this only satisfies registration.
const REGISTRATION_BASE_URL: &str = "https://aiplatform.googleapis.com/v1";

pub fn register_vertex_model_garden(
    api: &mut ExtensionApi,
    resolve_access: ResolveVertexAccess,
    stored_provider_env: Option<StoredProviderEnvReader>,
) {
    let auth = VertexModelGardenAmbientAuth {
        stored_provider_env,
    };
    let result = api.register_provider(
        VERTEX_MODEL_GARDEN_PROVIDER_ID,
        ProviderConfig {
            name: Some("Vertex AI Model Garden".into()),
            api: Some("openai-completions".into()),
            base_url: Some(REGISTRATION_BASE_URL.into()),
            // Ambient like the builtin google-vertex ADC lane (senpi#2327): available while the ADC chain
            // is present, never an explicitly configured provider, and never the automatic default over
            // one the user configured.
            ambient_auth: Some(Arc::new(auth)),
            models: Some(models::vertex_model_garden_models()),
            stream_simple: Some(Arc::new(move |model, context, options| {
                stream_vertex_model_garden(model, context, options, resolve_access.clone())
            })),
            fallback_eligible: Some(Arc::new(|| false)),
            ..Default::default()
        },
    );
    if let Err(error) = result {
        std::panic::panic_any(error);
    }
}

/// `vertexModelGardenExtension`.
#[derive(Clone, Copy, Default)]
pub struct VertexModelGarden;

impl Extension for VertexModelGarden {
    fn register(&self, api: &mut ExtensionApi) {
        register_vertex_model_garden(api, default_resolve_vertex_access(), None);
    }
}
