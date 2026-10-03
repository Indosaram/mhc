pub fn parse_enable_env(value:Option<&str>)->bool {
    let Some(value)=value.filter(|value|!value.is_empty()) else { return true; };
    !matches!(value.trim_matches(|character:char|matches!(character,'\u{0009}'..='\u{000d}'|'\u{0020}'|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}')).to_lowercase().as_str(),"0"|"false"|"no"|"off")
}
pub fn is_webfetch_enabled()->bool { parse_enable_env(std::env::var("PI_WEBFETCH").ok().as_deref()) }
pub struct WebfetchExtension;
impl maho_ext_api::Extension for WebfetchExtension {
    fn register(&self,api:&mut maho_ext_api::ExtensionApi) { register_webfetch_extension(api,is_webfetch_enabled()); }
}
pub fn register_webfetch_extension(api:&mut maho_ext_api::ExtensionApi,enabled:bool) {
    if !enabled { return; }
    if let Err(error)=api.register_tool_with_renderers(crate::webfetch::tool::create_webfetch_tool(),crate::webfetch::renderers::renderers()) { std::panic::panic_any(error); }
    for event in [maho_ext_api::EventKind::SessionStart,maho_ext_api::EventKind::SessionShutdown] {
        api.on(event,std::sync::Arc::new(|_event,ctx|Box::pin(async move {
            if ctx.has_ui { ctx.ui.set_status("pi-webfetch",None); ctx.ui.set_widget("pi-webfetch",None,Default::default()); }
            Ok(maho_ext_api::EventResult::None)
        })));
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn native_registration_respects_enable_gate_and_installs_lifecycle_hooks() {
        let mut api=maho_ext_api::ExtensionApi::new(maho_ext_api::LoadedExtension::new("webfetch",std::path::PathBuf::new(),Default::default()),Default::default(),Default::default(),Default::default());
        register_webfetch_extension(&mut api,false); assert!(api.registered.tools.is_empty()); assert!(api.registered.handlers.is_empty());
        register_webfetch_extension(&mut api,true); assert_eq!(api.registered.tools[0].definition.name,"webfetch"); assert_eq!(api.registered.handlers[&maho_ext_api::EventKind::SessionStart].len(),1); assert_eq!(api.registered.handlers[&maho_ext_api::EventKind::SessionShutdown].len(),1);
    }
    #[test] fn disables_only_explicit_false_values() { for value in ["0"," FALSE ","no","\u{feff}off\u{feff}"] { assert!(!parse_enable_env(Some(value))); } for value in [None,Some(""),Some("yes"),Some("unknown"),Some(" "),Some("\u{0085}off")] { assert!(parse_enable_env(value)); } }
}
