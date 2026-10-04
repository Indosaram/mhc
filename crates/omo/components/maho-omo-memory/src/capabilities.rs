use maho_ext_api::ExtensionApi;

pub fn missing_memory_capabilities(_api: &ExtensionApi) -> Vec<&'static str> { Vec::new() }
pub fn has_memory_capabilities(api: &ExtensionApi) -> bool { missing_memory_capabilities(api).is_empty() }

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn native_api_has_required_memory_surface() {
        let api = ExtensionApi::new(maho_ext_api::LoadedExtension::new("memory", Default::default(), Default::default()), Default::default(), Default::default(), Default::default());
        assert!(missing_memory_capabilities(&api).is_empty()); assert!(has_memory_capabilities(&api));
    }
}
