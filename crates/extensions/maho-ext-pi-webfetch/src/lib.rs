pub mod webfetch {
    pub mod content;
    pub mod errors;
    pub mod fetcher;
    pub mod tool;
    pub mod renderers;
}

use maho_ext_api::{EventKind, EventResult, Extension, ExtensionApi};
use std::sync::Arc;

pub fn is_webfetch_enabled(value: Option<&str>) -> bool {
    !matches!(value.unwrap_or("").trim().to_lowercase().as_str(), "0" | "false" | "no" | "off")
}

pub struct WebfetchExtension;
impl Extension for WebfetchExtension {
    fn register(&self, api: &mut ExtensionApi) {
        self.register_with_env(api, std::env::var("PI_WEBFETCH").ok().as_deref());
    }
}
impl WebfetchExtension {
    pub fn register_with_env(&self, api: &mut ExtensionApi, value: Option<&str>) {
        if is_webfetch_enabled(value) { self.register_enabled(api); }
    }
    pub fn register_enabled(&self, api: &mut ExtensionApi) {
        if let Err(error) = api.register_tool_with_extension_context(
            webfetch::tool::definition(),
            Arc::new(|_, params, signal, update, _| Box::pin(async move {
                webfetch::tool::execute(params, signal, update).await
            })),
        ) {
            std::panic::panic_any(error);
        }
        api.registered.tool_renderers.insert("webfetch".into(), Arc::new(webfetch::renderers::renderers()));
        for event in [EventKind::SessionStart, EventKind::SessionShutdown] {
            api.on(event, Arc::new(|_, context| Box::pin(async move {
                if context.has_ui {
                    context.ui.set_status("pi-webfetch", None);
                    context.ui.set_widget("pi-webfetch", None, Default::default());
                }
                Ok(EventResult::None)
            })));
        }
    }
}
