use std::collections::HashMap;

use super::resolve::KernelToolGrant;

#[derive(Default)]
pub struct KernelToolBindingRegistry { bindings: HashMap<String, KernelToolGrant> }

impl KernelToolBindingRegistry {
    pub fn bind(&mut self, key: impl Into<String>, grant: KernelToolGrant) { self.bindings.insert(key.into(), grant); }
    pub fn get(&self, key: &str) -> Option<&KernelToolGrant> { self.bindings.get(key) }
    pub fn release(&mut self, key: &str) { self.bindings.remove(key); }
    pub fn release_all(&mut self) { self.bindings.clear(); }
    pub fn keys(&self) -> Vec<String> { self.bindings.keys().cloned().collect() }
    pub fn len(&self) -> usize { self.bindings.len() }
    pub fn is_empty(&self) -> bool { self.bindings.is_empty() }
}

pub fn create_kernel_tool_bindings() -> KernelToolBindingRegistry { KernelToolBindingRegistry::default() }
