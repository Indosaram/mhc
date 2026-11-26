//! Port of senpi packages/coding-agent/src/core/permission-decision.ts.
//!
//! Binds the per-decision remote approval-mode rule (`omo#12965`) for live sessions.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

use crate::turn_origin::deciding_turn_is_delegated;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermissionPresetName {
    Guarded,
    FullAccess,
    Workspace,
    AcceptEdits,
    Auto,
    ReadOnly,
    Ask,
}

impl PermissionPresetName {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Guarded => "guarded",
            Self::FullAccess => "full-access",
            Self::Workspace => "workspace",
            Self::AcceptEdits => "accept-edits",
            Self::Auto => "auto",
            Self::ReadOnly => "read-only",
            Self::Ask => "ask",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "guarded" => Some(Self::Guarded),
            "full-access" => Some(Self::FullAccess),
            "workspace" => Some(Self::Workspace),
            "accept-edits" => Some(Self::AcceptEdits),
            "auto" => Some(Self::Auto),
            "read-only" => Some(Self::ReadOnly),
            "ask" => Some(Self::Ask),
            _ => None,
        }
    }
}

pub struct DecisionSession {
    pub session_id: String,
    pub parent_session_id: Option<String>,
    pub named_preset: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    pub launch_flag_named: Arc<dyn Fn() -> bool + Send + Sync>,
    pub recorded_preset: Arc<dyn Fn() -> Option<String> + Send + Sync>,
}

static DECISION_SESSIONS: LazyLock<Mutex<HashMap<String, Arc<DecisionSession>>>> =
    LazyLock::new(Default::default);

type OverrideHook =
    Arc<dyn Fn(&str, Arc<dyn Fn() -> Option<PermissionPresetName> + Send + Sync>) + Send + Sync>;
static OVERRIDE_HOOK: LazyLock<Mutex<Option<OverrideHook>>> = LazyLock::new(Default::default);

pub fn register_decision_preset_override_hook(hook: OverrideHook) {
    *OVERRIDE_HOOK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(hook);
}

pub fn bind_decision_preset(session: DecisionSession) {
    let session_id = session.session_id.clone();
    let arc = Arc::new(session);
    DECISION_SESSIONS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(session_id.clone(), Arc::clone(&arc));

    let hook = OVERRIDE_HOOK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    if let Some(hook) = hook {
        let resolver = Arc::new(move || decision_preset(&arc, &[]));
        hook(&session_id, resolver);
    }
}

pub fn release_decision_preset(session_id: &str) {
    DECISION_SESSIONS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(session_id);
}

pub fn decision_preset_for_session(session_id: &str) -> Option<PermissionPresetName> {
    let session = DECISION_SESSIONS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(session_id)
        .cloned()?;
    decision_preset(&session, &[])
}

fn decision_preset(
    session: &DecisionSession,
    visited_ids: &[String],
) -> Option<PermissionPresetName> {
    if (session.named_preset)().is_some() {
        return None;
    }
    if deciding_turn_is_delegated(&session.session_id) {
        return Some(delegated_preset(session));
    }
    let parent_id = session.parent_session_id.as_deref()?;
    let parent = DECISION_SESSIONS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(parent_id)
        .cloned();
    let Some(parent) = parent else {
        return if (session.launch_flag_named)() {
            None
        } else {
            Some(PermissionPresetName::Ask)
        };
    };
    if parent_id == session.session_id || visited_ids.iter().any(|v| v == parent_id) {
        return Some(PermissionPresetName::Ask);
    }
    let mut next_visited = visited_ids.to_vec();
    next_visited.push(session.session_id.clone());
    effective_preset_of(&parent, &next_visited)
}

fn effective_preset_of(
    session: &DecisionSession,
    visited_ids: &[String],
) -> Option<PermissionPresetName> {
    if let Some(named) = (session.named_preset)() {
        return PermissionPresetName::parse(&named);
    }
    decision_preset(session, visited_ids)
}

fn delegated_preset(session: &DecisionSession) -> PermissionPresetName {
    if (session.launch_flag_named)() {
        return PermissionPresetName::Ask;
    }
    (session.recorded_preset)()
        .as_deref()
        .and_then(PermissionPresetName::parse)
        .unwrap_or(PermissionPresetName::Ask)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_hook_can_replace_itself_during_binding() {
        const CHILD: &str = "MHC_TEST_PERMISSION_HOOK_CHILD";
        if std::env::var_os(CHILD).is_some() {
            let (sender, receiver) = std::sync::mpsc::channel();
            register_decision_preset_override_hook(Arc::new(move |_, _| {
                let next = sender.clone();
                register_decision_preset_override_hook(Arc::new(move |_, _| {
                    next.send(2).expect("observe replacement hook");
                }));
                sender.send(1).expect("observe original hook");
            }));
            let make_session = |session_id: &str| DecisionSession {
                session_id: session_id.to_owned(),
                parent_session_id: None,
                named_preset: Arc::new(|| None),
                launch_flag_named: Arc::new(|| false),
                recorded_preset: Arc::new(|| None),
            };
            std::thread::spawn(move || bind_decision_preset(make_session("first")));
            let timeout = std::time::Duration::from_secs(5);
            assert_eq!(receiver.recv_timeout(timeout).expect("original hook"), 1);
            bind_decision_preset(make_session("second"));
            assert_eq!(receiver.recv_timeout(timeout).expect("replacement hook"), 2);
            return;
        }
        let child = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .arg("override_hook_can_replace_itself_during_binding")
            .arg("--nocapture")
            .env(CHILD, "1")
            .output()
            .expect("run isolated test process");
        assert!(
            child.status.success(),
            "child failed: {}\n{}",
            String::from_utf8_lossy(&child.stdout),
            String::from_utf8_lossy(&child.stderr)
        );
    }
}
