pub mod index;
pub struct GoalExtension{
    pub resolve:goal::lifecycle::GoalStoreResolver,
    pub send:goal::lifecycle::GoalContinuationSender,
}
impl maho_ext_api::Extension for GoalExtension{
    fn register(&self,api:&mut maho_ext_api::ExtensionApi){
        if let Err(error)=index::register_goal_extension(api,self.resolve.clone(),self.send.clone()){
            std::panic::panic_any(error);
        }
    }
}
pub mod goal {
    pub mod command;
    pub mod command_registration;
    pub mod tool_registration;
    pub mod continuation;
    pub mod store;
    pub mod format;
    pub mod prompt;
    pub mod ui;
    pub mod lifecycle;
    pub mod errors;
    pub mod transitions;
    pub mod turn_usage;
    pub mod types;
    pub mod validation;
}
