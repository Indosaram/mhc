pub mod api_key;
pub mod auth_server;
pub mod catalog;
pub mod commands;
pub mod contract;
pub mod converters;
pub mod core;
pub mod cost;
pub mod json_schema;
pub mod models;
pub mod oauth;
pub mod overflow;
pub mod pricing;
pub mod quota;
pub mod quota_command;
pub mod quota_format;
pub mod quota_types;
pub mod runtime;
pub mod transport;
pub mod types;

use maho_ext_api::{Extension, ExtensionApi, ProviderConfig};
use std::sync::Arc;

pub struct CommandCodeProvider {
    pub env: contract::ProviderEnv,
    pub router: Arc<transport::TransportRouter>,
}

impl Default for CommandCodeProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl CommandCodeProvider {
    pub fn new() -> Self {
        Self {
            env: contract::ProviderEnv::from_process(),
            router: Arc::new(transport::TransportRouter::default()),
        }
    }
}

impl Extension for CommandCodeProvider {
    fn register(&self, api: &mut ExtensionApi) {
        let models = runtime::discover_models(&self.env);
        let headers = contract::zdr_headers();
        let config = ProviderConfig {
            name: Some(contract::PROVIDER_NAME.to_owned()),
            api: Some("commandcode-custom".to_owned()),
            base_url: Some(self.env.api_base.clone()),
            api_key: Some("$COMMAND_CODE_API_KEY".to_owned()),
            headers,
            oauth: Some(std::sync::Arc::new(
                maho_ext_host::oauth::ExtensionOAuthAdapter::new(std::sync::Arc::new(
                    oauth::CommandCodeOAuth {
                        api_base: contract::DEFAULT_API_BASE.to_owned(),
                    },
                )),
            )),
            models: Some(models),
            stream_simple: Some(Arc::new({
                let env = self.env.clone();
                let router = Arc::clone(&self.router);
                move |model, context, options| {
                    transport::stream_with_fallback(
                        model,
                        context,
                        options,
                        &env,
                        Arc::clone(&router),
                    )
                }
            })),
            ..Default::default()
        };
        api.register_provider(contract::PROVIDER_ID, config)
            .expect("Command Code provider registration");
        commands::register(api, self.env.clone());
    }
}
