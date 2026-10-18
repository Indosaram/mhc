pub const NATIVE_BADGE_STATUS_KEY: &str = "  omo-native";
pub const NATIVE_BADGE_TEXT: &str = "🌟 Maho Code";
pub const NATIVE_BADGE_DEV_BUILD_TEXT: &str = "☕ Maho Code";

pub fn package_dir_from_env(env:&std::collections::BTreeMap<String,String>)->Option<std::path::PathBuf>{env.get("OMO_PACKAGE_DIR").filter(|value|!value.trim().is_empty()).map(std::path::PathBuf::from)}

pub fn resolve_native_badge_text(package_dir: Option<&std::path::Path>) -> &'static str {
    let Some(package_dir) = package_dir else { return NATIVE_BADGE_TEXT };
    let Ok(contents) = std::fs::read_to_string(package_dir.join("package.json")) else { return NATIVE_BADGE_TEXT };
    let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&contents) else { return NATIVE_BADGE_TEXT };
    if manifest.get("omoBuild").and_then(|stamp| stamp.get("command")).and_then(serde_json::Value::as_str).is_some_and(|command| !command.is_empty()) {
        NATIVE_BADGE_DEV_BUILD_TEXT
    } else {
        NATIVE_BADGE_TEXT
    }
}

pub fn publish_native_badge(ui:&dyn maho_ext_api::ExtensionUi,package_dir:Option<&std::path::Path>){ui.set_status(NATIVE_BADGE_STATUS_KEY,Some(resolve_native_badge_text(package_dir)));}
pub fn clear_native_badge(ui:&dyn maho_ext_api::ExtensionUi){ui.set_status(NATIVE_BADGE_STATUS_KEY,None);}
pub fn register_native_badge(api:&mut maho_ext_api::ExtensionApi){let package_dir=package_dir_from_env(&std::env::vars().collect());let settled_dir=package_dir.clone();api.on(maho_ext_api::EventKind::SessionStart,std::sync::Arc::new(move|_,ctx|{publish_native_badge(ctx.ui.as_ref(),package_dir.as_deref());Box::pin(async{Ok(maho_ext_api::EventResult::None)})}));api.on(maho_ext_api::EventKind::AgentSettled,std::sync::Arc::new(move|_,ctx|{publish_native_badge(ctx.ui.as_ref(),settled_dir.as_deref());Box::pin(async{Ok(maho_ext_api::EventResult::None)})}));}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_and_dev_badges_follow_manifest_stamp() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(resolve_native_badge_text(Some(dir.path())), NATIVE_BADGE_TEXT);
        std::fs::write(dir.path().join("package.json"), r#"{"omoBuild":{"command":"dev"}}"#).unwrap();
        assert_eq!(resolve_native_badge_text(Some(dir.path())), NATIVE_BADGE_DEV_BUILD_TEXT);
    }
}
