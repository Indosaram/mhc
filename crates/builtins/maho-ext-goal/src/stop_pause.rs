use maho_ext_api::{ExtensionApi, ExtensionContext, ExtensionFailure, SessionEntry};
pub const GOAL_STOP_PAUSED_ENTRY: &str = "goal-stop-paused";
pub const GOAL_STOP_RESUMED_ENTRY: &str = "goal-stop-resumed";
/// Reduce durable ledger entries without confusing a run fence with a session Stop.
pub fn live_hold_kinds(entries: &[SessionEntry]) -> Vec<String> {
    let mut holds = std::collections::BTreeMap::new();
    for entry in entries {
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
pub fn stop_hold_live(ctx: &ExtensionContext) -> bool {
    match ctx.get_pause_state() {
        Ok(state) => state["holds"]
            .as_array()
            .is_some_and(|holds| holds.iter().any(|hold| hold["kind"] == "session_hold")),
        Err(error) => {
            maho_ext_api::diagnostic(&format!("Goal Stop pause state unavailable: {error}"));
            false
        }
    }
}
pub fn mark_stop_paused(pi: &ExtensionApi, goal_id: &str) -> Result<(), ExtensionFailure> {
    pi.append_entry(
        GOAL_STOP_PAUSED_ENTRY,
        Some(serde_json::json!({"goalId":goal_id})),
    )
}
pub fn mark_stop_resumed(pi: &ExtensionApi, goal_id: &str) -> Result<(), ExtensionFailure> {
    pi.append_entry(
        GOAL_STOP_RESUMED_ENTRY,
        Some(serde_json::json!({"goalId":goal_id})),
    )
}
pub fn paused_by_stop(ctx: &ExtensionContext, goal_id: &str) -> bool {
    ctx.session_manager
        .get_entries()
        .iter()
        .rev()
        .find_map(|entry| {
            if entry.kind != "custom" || entry.data["data"]["goalId"] != goal_id {
                return None;
            }
            match entry.data["customType"].as_str() {
                Some(GOAL_STOP_PAUSED_ENTRY) => Some(true),
                Some(GOAL_STOP_RESUMED_ENTRY) => Some(false),
                _ => None,
            }
        })
        .unwrap_or(false)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ledger_releases_exact_key() {
        let entry = |op: &str, key: &str, kind: &str| SessionEntry {
            id: String::new(),
            parent_id: None,
            timestamp: String::new(),
            kind: "custom".into(),
            data: serde_json::json!({"customType":"session-hold","data":{"op":op,"key":key,"kind":kind}}),
        };
        assert_eq!(
            live_hold_kinds(&[
                entry("add", "s", "session_hold"),
                entry("add", "f", "run_fence"),
                entry("release", "f", "")
            ]),
            vec!["session_hold"]
        );
    }
}

#[cfg(test)]
mod regression_tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    struct History(
        Mutex<Vec<SessionEntry>>,
        Arc<dyn maho_ext_api::ExtensionContextActions>,
    );
    impl maho_ext_api::ToolSessionManager for History {
        fn session_id(&self) -> &str {
            "s"
        }
        fn session_file(&self) -> Option<&std::path::Path> {
            None
        }
    }
    impl maho_ext_api::SessionManager for History {
        fn extension_context_actions(&self) -> Option<&dyn maho_ext_api::ExtensionContextActions> {
            Some(self.1.as_ref())
        }
        fn get_entries(&self) -> Vec<SessionEntry> {
            self.0.lock().unwrap().clone()
        }
        fn get_branch(&self) -> Vec<SessionEntry> {
            self.get_entries()
        }
        fn get_leaf_id(&self) -> Option<String> {
            None
        }
        fn get_session_name(&self) -> Option<String> {
            None
        }
    }
    fn entry(kind: &str, data: serde_json::Value) -> SessionEntry {
        SessionEntry {
            id: String::new(),
            parent_id: None,
            timestamp: String::new(),
            kind: kind.into(),
            data,
        }
    }
    #[tokio::test]
    async fn stop_pauses_then_release_resumes_only_stop_marked_goal() {
        let directory = tempfile::tempdir().unwrap();
        let reference = crate::types::GoalStoreRef {
            base_dir: directory.path().into(),
            thread_id: "s".into(),
        };
        let stored = reference.clone();
        let runtime =
            crate::runtime::GoalRuntime::new(Arc::new(move |_| stored.clone()), Arc::new(|| 0.0));
        let goal = crate::store::create_goal(&reference, "work", None, 0)
            .await
            .unwrap();
        let mut ctx = crate::test_context::context();
        let session = crate::test_context::bind_session(&mut ctx);
        // Match issue-3811-goal-stop-pause.test.ts: a Stop adds a live hold;
        // abort alone is a plain interrupt and correctly blocks the goal.
        session.stop().await;
        let history = Arc::new(History(
            Mutex::new(vec![entry(
                "custom",
                serde_json::json!({"customType":"session-hold","data":{"op":"add","kind":"session_hold","key":"stop"}}),
            )]),
            session.extension_context_actions(),
        ));
        ctx.has_ui = false;
        ctx.session_manager = history.clone();
        let paused = runtime
            .event(&maho_ext_api::ExtensionEvent::SessionAbort, &ctx)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(paused.status, crate::types::GoalStatus::Paused);
        history.0.lock().unwrap().push(entry(
            "custom",
            serde_json::json!({"customType":GOAL_STOP_PAUSED_ENTRY,"data":{"goalId":goal.id}}),
        ));
        history.0.lock().unwrap().push(entry("custom",serde_json::json!({"customType":"session-hold","data":{"op":"release","keys":["stop"]}})));
        session.resume_session().await.unwrap();
        let resumed = runtime
            .event(
                &maho_ext_api::ExtensionEvent::HeldWakesReleased {
                    sources: Default::default(),
                },
                &ctx,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(resumed.status, crate::types::GoalStatus::Active);
        crate::store::update_goal(
            &reference,
            &crate::types::GoalUpdate {
                status: Some(crate::types::GoalStatus::Paused),
                ..Default::default()
            },
            crate::types::GoalUpdateSource::User,
            1,
        )
        .await
        .unwrap();
        history.0.lock().unwrap().push(entry(
            "custom",
            serde_json::json!({"customType":GOAL_STOP_RESUMED_ENTRY,"data":{"goalId":goal.id}}),
        ));
        let explicit = runtime
            .event(
                &maho_ext_api::ExtensionEvent::HeldWakesReleased {
                    sources: Default::default(),
                },
                &ctx,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(explicit.status, crate::types::GoalStatus::Paused);
    }
}
