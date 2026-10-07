//! Native pi-rules modules, ported from the pinned extension.
pub mod config;
pub mod commands;
pub mod index;
pub struct RulesExtension;
impl maho_ext_api::Extension for RulesExtension{
    fn register(&self,api:&mut maho_ext_api::ExtensionApi){index::register_rule_injection_hooks(api);}
}
pub mod ui {
    pub mod dynamic_border;
    pub mod rules_banner;
}
pub mod rules {
    pub mod cache;
    pub mod constants;
    pub mod engine;
    pub mod errors;
    pub mod finder;
    pub mod formatter;
    pub mod matcher;
    pub mod ordering;
    pub mod parser;
    pub mod picomatch;
    pub mod project_root;
    pub mod scanner;
    pub mod tool_paths;
    pub mod truncator;
    pub mod types;
}
