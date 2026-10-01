use senpi_task::host::SenpiModelRegistry;
pub trait MemoryModelRegistryContext{fn model_registry(&self)->Option<&dyn SenpiModelRegistry>;}
pub fn resolve_memory_model_registry(context:Option<&dyn MemoryModelRegistryContext>)->Option<&dyn SenpiModelRegistry>{context?.model_registry()}
#[cfg(test)]
mod tests{
    use super::*;
    struct Context{registry:bool}
    impl SenpiModelRegistry for Context{fn get_available(&self)->Result<serde_json::Value,senpi_task::host::HostError>{Ok(serde_json::json!([]))}fn find(&self,_:&str,_:&str)->Option<serde_json::Value>{None}}
    impl MemoryModelRegistryContext for Context{fn model_registry(&self)->Option<&dyn SenpiModelRegistry>{self.registry.then_some(self)}}
    #[test]fn complete_native_registry_returned_by_identity(){let context=Context{registry:true};let registry=resolve_memory_model_registry(Some(&context)).unwrap();assert!(std::ptr::eq(registry,&context as &dyn SenpiModelRegistry));assert_eq!(registry.get_available().unwrap(),serde_json::json!([]));assert_eq!(registry.find("provider","id"),None);}
    #[test]fn missing_capability_rejected(){assert!(resolve_memory_model_registry(None).is_none());assert!(resolve_memory_model_registry(Some(&Context{registry:false})).is_none());}
}
