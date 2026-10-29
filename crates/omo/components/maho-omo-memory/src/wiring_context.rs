use maho_ext_api::{ExtensionContext, ExtensionUi, SessionManager};

pub fn session_id_from(context: Option<&ExtensionContext>) -> Option<&str> {
    context.map(|context| context.session_manager.session_id()).filter(|id| !id.is_empty())
}

pub fn branch_entry_count(manager: Option<&dyn SessionManager>) -> usize {
    manager.map_or(0, |manager| manager.get_entries().len())
}

pub fn read_ui(context: Option<&ExtensionContext>) -> Option<&dyn ExtensionUi> {
    context.map(|context| context.ui.as_ref())
}

pub fn is_record(value: &serde_json::Value) -> bool { value.is_object() }

#[cfg(test)]
mod tests {
    use super::*;
    struct Session;
    impl maho_ext_api::ToolSessionManager for Session {
        fn session_id(&self) -> &str { "session" }
        fn session_file(&self) -> Option<&std::path::Path> { None }
    }
    impl SessionManager for Session {
        fn get_entries(&self) -> Vec<maho_ext_api::SessionEntry> { vec![maho_ext_api::SessionEntry { id: "entry".into(), parent_id: None, timestamp: "now".into(), kind: "user".into(), data: maho_ext_api::JsonValue::Null }] }
        fn get_branch(&self) -> Vec<maho_ext_api::SessionEntry> { vec![] }
        fn get_leaf_id(&self) -> Option<String> { None }
        fn get_session_name(&self) -> Option<String> { None }
    }
    #[test] fn absent_context_is_noop() { assert!(session_id_from(None).is_none()); assert!(read_ui(None).is_none()); assert_eq!(branch_entry_count(None), 0); }
    #[test] fn branch_count_uses_all_entries_not_current_branch() { assert_eq!(branch_entry_count(Some(&Session)), 1); }
    #[test] fn record_rejects_arrays_null_and_primitives() { assert!(is_record(&serde_json::json!({}))); for value in [serde_json::Value::Null, serde_json::json!([]), serde_json::json!("text"), serde_json::json!(1)] { assert!(!is_record(&value)); } }
}
