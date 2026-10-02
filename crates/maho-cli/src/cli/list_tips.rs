pub fn collect_tips() -> serde_json::Value {
    let bindings = maho_core::keybindings::KeybindingsManager::new(Default::default(), None);
    let keys = |binding: &str| bindings.inner().get_keys(binding).join("/");
    serde_json::Value::Array(maho_interactive::tips::registry::TIP_DEFINITIONS.iter().map(|tip| {
        let mut result = serde_json::json!({"id": tip.id, "text": (tip.render)(&keys)});
        if let Some(command) = tip.requires_command { result["requiresCommand"] = serde_json::Value::String(command.to_owned()); } result
    }).collect())
}
