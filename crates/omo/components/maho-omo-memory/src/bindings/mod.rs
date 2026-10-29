pub mod entry_renderer;

pub use entry_renderer::render_memory_binding_entry;

pub fn register_memory_binding_renderer(api: &mut maho_ext_api::ExtensionApi) {
    api.register_entry_renderer(
        crate::binding::MEMORY_BINDING_CUSTOM_TYPE,
        std::sync::Arc::new(|_, _, _| entry_renderer::render_memory_binding_entry()),
        Default::default(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binding_renderer_is_registered_and_hidden() {
        let mut api = maho_ext_api::ExtensionApi::new(
            maho_ext_api::LoadedExtension::new("memory-bindings", Default::default(), Default::default()),
            Default::default(),
            Default::default(),
            Default::default(),
        );

        register_memory_binding_renderer(&mut api);

        let renderer = api
            .registered
            .entry_renderers
            .get(crate::binding::MEMORY_BINDING_CUSTOM_TYPE)
            .unwrap();
        let entry = maho_ext_api::SessionEntry {
            id: "entry".to_string(),
            parent_id: None,
            timestamp: "now".to_string(),
            kind: "custom".to_string(),
            data: serde_json::json!({}),
        };
        assert!(
            renderer(
                &entry,
                &maho_ext_api::EntryRenderOptions::default(),
                &Default::default()
            )
            .is_none()
        );
    }
}
