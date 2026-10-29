//! What a reflection launch needs from the session that triggered it, bound to the run (pin
//! `launch-facts.ts`).
//!
//! A launch reads its facts asynchronously, long after the trigger handler returned. By then the
//! triggering ctx may be retired, or another session's ctx may be the current one (#10339). Reading a
//! shared "current" ctx there is a cross-session leak. So each launch runs inside a scope bound to its
//! run:
//! - a trigger launch carries an immutable snapshot taken from the triggering session's live ctx, and
//!   reads only that;
//! - any other launch (bind-time reconcile, a queued follow-up, a dream) carries the run's own
//!   conversation ids, and session-specific facts are taken from the current ctx only when that ctx
//!   IS one of those conversations. Otherwise the launch gets none (only the process-wide model
//!   registry, which no session owns).
use std::sync::Arc;

use crate::session_model_resolver::ReflectionSessionModel;

/// What a reflection launch needs from the session that triggered it.
#[derive(Clone)]
pub struct LaunchFacts {
    pub model_registry: Option<Arc<dyn maho_ext_api::ModelRegistry>>,
    pub session_model: Option<ReflectionSessionModel>,
    pub parent_context_tokens: Option<f64>,
    pub parent_session_file: Option<String>,
    pub parent_cache_reusable: bool,
    pub agent_dir: Option<std::path::PathBuf>,
}

pub const NO_LAUNCH_FACTS: LaunchFacts = LaunchFacts {
    model_registry: None,
    session_model: None,
    parent_context_tokens: None,
    parent_session_file: None,
    parent_cache_reusable: false,
    agent_dir: None,
};

/// Reads the launch facts from a live extension ctx. A retired ctx fails with its stale-guard error;
/// callers check it with [`crate::stale_extension_context::is_stale_extension_context_error`].
pub fn read_launch_facts(
    context: &maho_ext_api::ExtensionContext,
) -> Result<LaunchFacts, maho_ext_api::ExtensionFailure> {
    Ok(LaunchFacts {
        model_registry: Some(
            crate::model_registry_resolver::resolve_native_memory_model_registry(context).0,
        ),
        session_model: crate::session_model_resolver::resolve_native_memory_session_model(context),
        parent_context_tokens:
            crate::session_context_resolver::resolve_native_parent_context_tokens(context)?,
        parent_session_file: crate::session_context_resolver::resolve_native_parent_session_file(
            context,
        ),
        parent_cache_reusable: context
            .session_manager
            .get_usage_totals()
            .and_then(|totals| totals.get("cacheRead")?.as_f64())
            .is_some_and(|read| read > 0.0),
        agent_dir: Some(context.agent_dir.clone()),
    })
}

/// The facts a launch may use, bound to the run.
#[derive(Clone, Default)]
pub struct LaunchScope {
    pub conversation_ids: Vec<String>,
    pub snapshot: Option<LaunchFacts>,
}

tokio::task_local! {
    static LAUNCH_SCOPE: LaunchScope;
}

/// Runs `launch` inside `scope`; the scope is copied, so later changes to the caller's value never
/// reach the launch (the snapshot is frozen at this point).
pub async fn with_launch_scope<T>(
    scope: &LaunchScope,
    launch: impl std::future::Future<Output = T>,
) -> T {
    LAUNCH_SCOPE.scope(scope.clone(), launch).await
}

/// The scope of the launch running on this task, if any.
pub fn current_launch_scope() -> Option<LaunchScope> {
    LAUNCH_SCOPE.try_with(Clone::clone).ok()
}

/// Facts for a launch from the current ctx, restricted to what the run's own conversations may see.
pub fn facts_for_scope(
    scope: &LaunchScope,
    current_session_id: Option<&str>,
    live: &LaunchFacts,
) -> LaunchFacts {
    if let Some(snapshot) = &scope.snapshot {
        return snapshot.clone();
    }
    if current_session_id.is_some_and(|id| {
        scope
            .conversation_ids
            .iter()
            .any(|conversation| conversation == id)
    }) {
        return live.clone();
    }
    LaunchFacts {
        model_registry: live.model_registry.clone(),
        ..NO_LAUNCH_FACTS
    }
}

/// Same conversations in the same count: both lists have one length and every id of `left` is in `right`.
pub fn same_conversations(left: &[String], right: &[String]) -> bool {
    left.len() == right.len()
        && left.iter().all(|id| right.contains(id))
        && right.iter().all(|id| left.contains(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn live() -> LaunchFacts {
        LaunchFacts {
            parent_context_tokens: Some(10.0),
            parent_session_file: Some("live.jsonl".into()),
            parent_cache_reusable: true,
            agent_dir: Some("/agent".into()),
            ..NO_LAUNCH_FACTS
        }
    }

    #[test]
    fn a_snapshot_wins_over_the_live_ctx() {
        let snapshot = LaunchFacts {
            parent_session_file: Some("snap.jsonl".into()),
            ..NO_LAUNCH_FACTS
        };
        let scope = LaunchScope {
            conversation_ids: vec!["a".into()],
            snapshot: Some(snapshot),
        };
        assert_eq!(
            facts_for_scope(&scope, Some("a"), &live())
                .parent_session_file
                .as_deref(),
            Some("snap.jsonl")
        );
    }

    #[test]
    fn the_live_ctx_is_used_only_when_it_is_one_of_the_run_conversations() {
        let scope = LaunchScope {
            conversation_ids: vec!["a".into(), "b".into()],
            snapshot: None,
        };
        assert_eq!(
            facts_for_scope(&scope, Some("b"), &live())
                .parent_session_file
                .as_deref(),
            Some("live.jsonl")
        );
        let foreign = facts_for_scope(&scope, Some("other"), &live());
        assert!(
            foreign.parent_session_file.is_none()
                && foreign.agent_dir.is_none()
                && !foreign.parent_cache_reusable
        );
        assert!(
            facts_for_scope(&scope, None, &live())
                .parent_context_tokens
                .is_none()
        );
    }

    #[test]
    fn conversations_match_by_count_and_membership() {
        let ids = |items: &[&str]| items.iter().map(|id| (*id).to_owned()).collect::<Vec<_>>();
        assert!(same_conversations(&ids(&["a", "b"]), &ids(&["b", "a"])));
        assert!(!same_conversations(&ids(&["a"]), &ids(&["a", "b"])));
        assert!(!same_conversations(&ids(&["a", "a"]), &ids(&["a", "b"])));
    }

    #[tokio::test]
    async fn the_scope_is_visible_inside_the_launch_only() {
        assert!(current_launch_scope().is_none());
        let scope = LaunchScope {
            conversation_ids: vec!["a".into()],
            snapshot: None,
        };
        let seen = with_launch_scope(&scope, async {
            current_launch_scope().map(|scope| scope.conversation_ids)
        })
        .await;
        assert_eq!(seen, Some(vec!["a".to_owned()]));
        assert!(current_launch_scope().is_none());
    }
}
