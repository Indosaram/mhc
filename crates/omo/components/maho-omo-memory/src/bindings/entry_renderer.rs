use maho_ext_api::Component;

pub fn render_memory_binding_entry() -> Option<Box<dyn Component>> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persisted_binding_stays_hidden() {
        assert!(render_memory_binding_entry().is_none());
    }
}
