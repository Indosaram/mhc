use maho_ext_api::{ExtensionContext, ExtensionFailure};
fn keyed_kinds(ctx: &ExtensionContext) -> Vec<(String, String)> {
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
    holds.into_iter().collect()
}
fn kinds(ctx: &ExtensionContext) -> Vec<String> {
    keyed_kinds(ctx).into_iter().map(|(_, kind)| kind).collect()
}
pub fn session_held(ctx: &ExtensionContext) -> Result<bool, ExtensionFailure> {
    Ok(kinds(ctx).iter().any(|kind| kind != "run_fence") || ctx.engine_turns_held()?)
}
pub fn user_stopped(ctx: &ExtensionContext) -> bool {
    keyed_kinds(ctx)
        .iter()
        .any(|(key, kind)| kind == "session_hold" && key != "interrupted-run")
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_ext_api::{SessionEntry, SessionManager, ToolSessionManager};
    struct Ledger(Vec<SessionEntry>);
    impl ToolSessionManager for Ledger {
        fn session_id(&self) -> &str {
            "s"
        }
        fn session_file(&self) -> Option<&std::path::Path> {
            None
        }
    }
    impl SessionManager for Ledger {
        fn get_entries(&self) -> Vec<SessionEntry> {
            self.0.clone()
        }
        fn get_branch(&self) -> Vec<SessionEntry> {
            self.0.clone()
        }
        fn get_leaf_id(&self) -> Option<String> {
            None
        }
        fn get_session_name(&self) -> Option<String> {
            None
        }
    }
    fn stopped(holds: &[(&str, &str)]) -> bool {
        let mut ctx = crate::controller_tests::context();
        ctx.session_manager = std::sync::Arc::new(Ledger(
            holds
                .iter()
                .map(|(key, kind)| SessionEntry {
                    id: String::new(),
                    parent_id: None,
                    timestamp: String::new(),
                    kind: "custom".into(),
                    data: serde_json::json!({"customType":"session-hold","data":{"op":"add","key":key,"kind":kind}}),
                })
                .collect(),
        ));
        user_stopped(&ctx)
    }
    // senpi 8c5b96cb9b loop/session-hold.ts: the interrupted-run gate is not the user's Stop.
    #[test]
    fn interrupted_run_gate_is_not_a_user_stop() {
        assert!(!stopped(&[("interrupted-run", "session_hold")]));
        assert!(stopped(&[("stop", "session_hold")]));
        assert!(stopped(&[
            ("interrupted-run", "session_hold"),
            ("session_hold:2", "session_hold")
        ]));
        assert!(!stopped(&[("run:task:a", "run_fence")]));
    }
}
