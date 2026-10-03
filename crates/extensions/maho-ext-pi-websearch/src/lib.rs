pub mod index;
pub struct WebsearchExtension {
    pub native_registry: Option<std::sync::Arc<dyn websearch::native::NativeModelRegistry>>,
}
impl maho_ext_api::Extension for WebsearchExtension {
    fn register(&self, api: &mut maho_ext_api::ExtensionApi) {
        let config = index::register_search_lifecycle(api);
        if let Err(error) = websearch::tool::register_search_tool(api, config, self.native_registry.clone()) {
            std::panic::panic_any(error);
        }
        api.registered.tool_renderers.insert("web_search".into(), std::sync::Arc::new(websearch::renderers::registered_renderers()));
    }
}
pub mod websearch {
    pub mod config;
    pub mod native;
    pub mod provider_endpoints;
    pub mod types;
    pub mod providers;
    pub mod search;
    pub mod tool;
    pub mod renderers;
}
