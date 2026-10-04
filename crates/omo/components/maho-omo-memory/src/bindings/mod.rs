pub fn register_memory_binding_renderer(api: &mut maho_ext_api::ExtensionApi) {
    api.register_entry_renderer(crate::binding::MEMORY_BINDING_CUSTOM_TYPE,
        std::sync::Arc::new(|_, _, _| None), Default::default());
}
