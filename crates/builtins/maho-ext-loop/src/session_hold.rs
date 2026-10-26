use maho_ext_api::{ExtensionContext, ExtensionFailure};
fn kinds(ctx: &ExtensionContext) -> Vec<String> {
    let mut holds = std::collections::BTreeMap::new();
    for entry in ctx.session_manager.get_entries() {
        if entry.kind != "custom" || entry.data["customType"] != "session-hold" {
            continue;
        }
        let data = &entry.data["data"];
        let key = data["key"].as_str().unwrap_or("").to_owned();
        match data["op"].as_str() {
            Some("add") => {
                if let Some(kind) = data["kind"].as_str() {
                    holds.insert(key, kind.to_owned());
                }
            }
            Some("release") => {
                if let Some(keys) = data["keys"].as_array() {
                    for key in keys.iter().filter_map(serde_json::Value::as_str) {
                        holds.remove(key);
                    }
                } else {
                    holds.remove(&key);
                }
            }
            _ => (),
        }
    }
    holds.into_values().collect()
}
pub fn session_held(ctx: &ExtensionContext) -> Result<bool, ExtensionFailure> {
    Ok(kinds(ctx).iter().any(|kind| kind != "run_fence") || ctx.engine_turns_held()?)
}
pub fn user_stopped(ctx: &ExtensionContext) -> bool {
    kinds(ctx).iter().any(|kind| kind == "session_hold")
}
