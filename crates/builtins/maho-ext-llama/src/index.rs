//! Port of `packages/coding-agent/src/extensions/llama/index.ts` (pin `fe8c564b`).

use std::sync::Arc;

use maho_ext_api::{Extension, ExtensionApi, ExtensionCommandContext, ExtensionFuture, NotificationType};
use serde_json::Value;

use crate::client::{LlamaClient, normalize_llama_server_url};
use crate::provider::{LLAMA_PROVIDER_ID, llama_provider_config};

fn configured_server_url() -> String {
    std::env::var("LLAMA_BASE_URL")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .and_then(|value| normalize_llama_server_url(&value).ok())
        .unwrap_or_else(|| crate::provider::DEFAULT_LLAMA_SERVER_URL.to_owned())
}

async fn run_list(ctx: &ExtensionCommandContext) -> Result<(), String> {
    let server_url = configured_server_url();
    let client = LlamaClient::new(&server_url, None)?;
    let models = client.list(false, None).await?;
    let ids: Vec<String> = models.iter().map(|model| model.id.clone()).collect();
    let message = if ids.is_empty() {
        format!("llama.cpp at {server_url}: no models")
    } else {
        format!("llama.cpp at {server_url}:\n{}", ids.join("\n"))
    };
    ctx.ui.notify(&message, NotificationType::Info);
    Ok(())
}

pub struct LlamaExtension;

impl Extension for LlamaExtension {
    fn register(&self, api: &mut ExtensionApi) {
        if let Err(error) = api.register_provider(LLAMA_PROVIDER_ID, llama_provider_config()) {
            api.register_command(
                "llama",
                Some("Manage llama.cpp router models".to_owned()),
                Some("[list]".to_owned()),
                Arc::new(move |_args, _ctx| Box::pin(async move { Err(error.clone()) }) as ExtensionFuture<'static, ()>),
            );
            return;
        }
        api.register_command_with_context(
            "llama",
            Some("Manage llama.cpp router models".to_owned()),
            Some("[list]".to_owned()),
            Arc::new(|_args, ctx| Box::pin(async move {
                if !ctx.has_ui {
                    ctx.ui.notify("No UI available", NotificationType::Info);
                    return Ok(());
                }
                if let Err(error) = run_list(ctx).await {
                    ctx.ui.notify(&error, NotificationType::Error);
                }
                Ok(())
            })),
        );
    }
}

pub fn llama() -> Box<dyn Extension> {
    Box::new(LlamaExtension)
}
