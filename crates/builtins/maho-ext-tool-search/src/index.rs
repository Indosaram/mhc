use crate::engine::document::{ToolSearchDocument,ToolSearchSource};
pub fn native_search_enabled(catalog:&[ToolSearchDocument],active:&[String],mcp_native_enabled:bool)->bool { catalog.iter().any(|document|match document.source { ToolSearchSource::Extension=>!active.contains(&document.name),ToolSearchSource::Mcp=>mcp_native_enabled }) }
pub fn is_deferrable(name:&str,catalog:&[ToolSearchDocument],active:&[String],mcp_native_enabled:bool)->bool { catalog.iter().find(|document|document.name==name).is_some_and(|document|match document.source { ToolSearchSource::Mcp=>mcp_native_enabled,ToolSearchSource::Extension=>!active.iter().any(|active|active==name) }) }
#[cfg(test)]
mod tests {
    use super::*;
    fn document(source:ToolSearchSource)->ToolSearchDocument { ToolSearchDocument{name:"read".into(),label:"Read".into(),aliases:vec![],description:None,search_text:None,keywords:vec![],source,group:"test".into(),owner_label:"test".into(),registration_id:"test".into()} }
    #[test] fn extension_deferral_requires_inactive_tool() { let catalog=[document(ToolSearchSource::Extension)]; assert!(native_search_enabled(&catalog,&[],false)); assert!(!native_search_enabled(&catalog,&["read".into()],true)); assert!(!is_deferrable("missing",&catalog,&[],true)); }
    #[test] fn mcp_deferral_uses_native_setting_not_active_set() { let catalog=[document(ToolSearchSource::Mcp)]; assert!(!is_deferrable("read",&catalog,&[],false)); assert!(is_deferrable("read",&catalog,&["read".into()],true)); }
}
