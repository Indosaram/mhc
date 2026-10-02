use senpi_task::host::SenpiModelRegistry;
pub trait MemoryModelRegistryContext{fn model_registry(&self)->Option<&dyn SenpiModelRegistry>;}
pub fn resolve_memory_model_registry(context:Option<&dyn MemoryModelRegistryContext>)->Option<&dyn SenpiModelRegistry>{context?.model_registry()}
pub struct NativeMemoryModelRegistry(pub std::sync::Arc<dyn maho_ext_api::ModelRegistry>);
impl SenpiModelRegistry for NativeMemoryModelRegistry{
    fn get_available(&self)->Result<serde_json::Value,senpi_task::host::HostError>{serde_json::to_value(self.0.get_available()).map_err(|error|senpi_task::host::HostError{message:error.to_string()})}
    fn find(&self,provider:&str,id:&str)->Option<serde_json::Value>{self.0.find(provider,id).and_then(|model|serde_json::to_value(model).ok())}
}
pub fn resolve_native_memory_model_registry(context:&maho_ext_api::ExtensionContext)->NativeMemoryModelRegistry{NativeMemoryModelRegistry(context.model_registry.clone())}
#[cfg(test)]
mod tests{
    use super::*;
    struct Context{registry:bool}
    impl SenpiModelRegistry for Context{fn get_available(&self)->Result<serde_json::Value,senpi_task::host::HostError>{Ok(serde_json::json!([]))}fn find(&self,_:&str,_:&str)->Option<serde_json::Value>{None}}
    impl MemoryModelRegistryContext for Context{fn model_registry(&self)->Option<&dyn SenpiModelRegistry>{self.registry.then_some(self)}}
    #[test]fn complete_native_registry_returned_by_identity(){let context=Context{registry:true};let registry=resolve_memory_model_registry(Some(&context)).unwrap();assert!(std::ptr::eq(registry,&context as &dyn SenpiModelRegistry));assert_eq!(registry.get_available().unwrap(),serde_json::json!([]));assert_eq!(registry.find("provider","id"),None);}
    #[test]fn missing_capability_rejected(){assert!(resolve_memory_model_registry(None).is_none());assert!(resolve_memory_model_registry(Some(&Context{registry:false})).is_none());}
}
