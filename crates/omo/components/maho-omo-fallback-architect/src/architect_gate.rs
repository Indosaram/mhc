use std::path::Path;
use maho_ext_api::ModelRegistry;
pub fn has_active_architect_category(cwd:&Path,registry:Option<&dyn ModelRegistry>)->bool {
    let result=maho_omo_config_resolution::load_senpi_omo_config(omo_config_core::LoadOmoConfigOptions{cwd:Some(cwd.to_string_lossy().into_owned()),..Default::default()});
    if let Some(architect)=result.config.get("categories").and_then(|v|v.get("architect")) { return architect["disable"]!=true; }
    let Some(registry)=registry else { return false; };
    struct Registry<'a>(&'a dyn ModelRegistry);
    impl senpi_task::host::SenpiModelRegistry for Registry<'_> {
        fn get_available(&self)->Result<serde_json::Value,senpi_task::host::HostError> { serde_json::to_value(self.0.get_available()).map_err(|e|senpi_task::host::HostError{message:e.to_string()}) }
        fn find(&self,provider:&str,id:&str)->Option<serde_json::Value> { self.0.find(provider,id).and_then(|m|serde_json::to_value(m).ok()) }
    }
    senpi_task::category::resolve_available_category_names(&result.config,&Registry(registry)).iter().any(|name|name=="architect")
}
