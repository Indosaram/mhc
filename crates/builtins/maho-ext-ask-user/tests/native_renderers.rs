use maho_ext_api::*;
use maho_ext_host::loader::{load_extensions,NativeExtensionFactory};
#[test]
fn loaded_question_tools_preserve_native_renderers_after_executor_registration(){
    let loaded=load_extensions(vec![NativeExtensionFactory{path:"<ask-renderers>".into(),source_info:SourceInfo::default(),extension:Box::new(maho_ext_ask_user::AskUser)}],std::path::Path::new("/tmp"),ExtensionSessionProfile::default());
    assert!(loaded.errors.is_empty());
    let extension=&loaded.extensions[0];
    for name in ["request_user_input","ask_user_question"]{
        let stored=extension.tool_renderers.get(name).expect("registered renderers");
        let renderers=stored.downcast_ref::<ToolRenderers<(),serde_json::Value>>().expect("exact renderer identity");
        assert!(renderers.render_call.is_some());assert!(renderers.render_result.is_some());
        assert!(loaded.runtime.extension_tool_executor("<ask-renderers>",name).is_some());
    }
}
