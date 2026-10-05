use std::sync::Arc;

use maho_ext_api::{Extension, ExtensionApi};

use crate::index::MemoryComponent;
use crate::wiring::MemoryWiring;
use crate::wiring_static::{register_memory_static, MemoryStaticOptions};
use crate::wiring_types::NativeMemoryWiringOptions;

pub struct MemoryExtensionOptions {
    pub component: Arc<MemoryComponent>,
    pub wiring: Arc<tokio::sync::Mutex<MemoryWiring>>,
    pub wiring_options: NativeMemoryWiringOptions,
    pub static_options: MemoryStaticOptions,
}

pub struct MemoryExtension {
    component: Arc<MemoryComponent>,
    wiring: Arc<tokio::sync::Mutex<MemoryWiring>>,
    wiring_options: NativeMemoryWiringOptions,
    static_options: MemoryStaticOptions,
}

impl MemoryExtension {
    pub fn new(options: MemoryExtensionOptions) -> Self {
        Self {
            component: options.component,
            wiring: options.wiring,
            wiring_options: options.wiring_options,
            static_options: options.static_options,
        }
    }

    pub fn component(&self) -> &Arc<MemoryComponent> {
        &self.component
    }
}

impl Extension for MemoryExtension {
    fn register(&self, api: &mut ExtensionApi) {
        let static_options = self.static_options.clone();
        let result = MemoryWiring::register_native(self.wiring.clone(), &self.component, api, self.wiring_options.clone(), |api| {
            register_memory_static(api, static_options);
        });
        if let Err(error) = result {
            std::panic::panic_any(format!("memory registration failed: {error}"));
        }
    }
}
