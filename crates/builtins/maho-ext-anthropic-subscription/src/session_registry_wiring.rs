use std::sync::Arc;
use maho_ext_api::{ExtensionApi, ExtensionEvent, EventKind, EventResult, SessionCompactEvent};
use crate::session_stream::SessionRegistry;

pub fn register(api: &mut ExtensionApi, registry: Arc<tokio::sync::Mutex<SessionRegistry>>) {
    let boundary = Arc::new(std::sync::Mutex::new(crate::session_commit_boundary::AssistantCommitBoundary::default()));
    for kind in [EventKind::SessionStart, EventKind::SessionCompact, EventKind::SessionTree, EventKind::ModelSelect, EventKind::ThinkingLevelSelect, EventKind::SessionShutdown, EventKind::SessionExtensionsRemoved, EventKind::MessageUpdate, EventKind::MessageEnd] {
        let registry = registry.clone(); let boundary = boundary.clone();
        let runtime = api.runtime.clone(); let cwd = api.cwd.clone(); let profile = api.profile.clone();
        api.on(kind, Arc::new(move |event, ctx| {
            let registry = registry.clone(); let boundary = boundary.clone();
            let runtime = runtime.clone(); let cwd = cwd.clone(); let profile = profile.clone(); Box::pin(async move {
                runtime.assert_active()?;
                let session = ctx.session_manager.session_id().to_owned();
                let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("clock").as_millis() as u64;
                let mut registry = registry.lock().await;
                runtime.assert_active()?;
                match event {
                    ExtensionEvent::SessionStart(event) if event.reason != maho_ext_api::SessionReason::Reload => {
                        registry.bindings.forget(&session); registry.bindings.remember_invalidation(&session, None);
                        if let Some(path) = ctx.session_manager.session_file() {
                            if event.reason == maho_ext_api::SessionReason::Fork {
                                crate::session_binding_store::delete_binding(path).map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))?;
                                registry.bindings.remember_invalidation(&session, Some("fork"));
                            } else if event.reason != maho_ext_api::SessionReason::New {
                                let branch: Vec<_> = ctx.session_manager.get_branch().into_iter().map(|entry| { let mut value = entry.data; value["id"] = serde_json::json!(entry.id); value["type"] = serde_json::json!(entry.kind); value }).collect();
                                registry.bindings.remember_invalidation(&session, crate::session_binding::invalidation_reason_from_branch(&branch));
                                if let Some(stored) = crate::session_binding_store::read_binding(path).map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))? {
                                    if stored.session_id == session && let Some(binding) = crate::session_binding::binding_from_stored_branch(&branch, &stored) { registry.bindings.remember(&session, &binding); }
                                    else { crate::session_binding_store::delete_binding(path).map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))?; }
                                }
                            }
                        }
                    },
                    ExtensionEvent::SessionCompact(SessionCompactEvent::Accepted { .. }) => {
                        crate::session_entry_annotations::annotate_pending_fork(registry.entries.get_mut(&session), "compaction", now);
                        registry.bindings.forget(&session); registry.bindings.remember_invalidation(&session, Some("compaction"));
                    },
                    ExtensionEvent::SessionTree { old_leaf_id: Some(old), new_leaf_id: Some(new), .. } => {
                        crate::session_entry_annotations::annotate_branch_info(registry.entries.get_mut(&session), &crate::session_entry_annotations::SessionBranchInfo { old_leaf_id: old.clone(), new_leaf_id: new.clone() }, now);
                        registry.bindings.forget(&session); registry.bindings.remember_invalidation(&session, Some("tree_changed"));
                    },
                    ExtensionEvent::ModelSelect(event) => {
                        if event.model.provider == "anthropic-subscription" {
                            if !crate::session_entry_annotations::switch_entry_model(registry.entries.get_mut(&session), &event.model.id, now).await.map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))? { registry.close(&session).await.map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))?; }
                        } else { registry.close(&session).await.map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))?; }
                    },
                    ExtensionEvent::ThinkingLevelSelect { .. } | ExtensionEvent::SessionShutdown(_) => { registry.close(&session).await.map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))?; },
                    ExtensionEvent::SessionExtensionsRemoved { .. } => { registry.close(&session).await.map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))?; registry.bindings.forget(&session); registry.bindings.remember_invalidation(&session, Some("extensions_removed")); },
                    ExtensionEvent::MessageUpdate { message, .. } => {
                        let message = serde_json::to_value(message).expect("message");
                        if message["role"] == "assistant" && registry.entries.get(&session).is_some_and(|entry| crate::session_commit_boundary::is_resident_assistant(&message, &entry.model_id)) { boundary.lock().expect("boundary").capture_provider_final(&session, &message); }
                    },
                    ExtensionEvent::MessageEnd { message } => {
                        let message = serde_json::to_value(message).expect("message");
                        if message["role"] == "assistant" {
                            if crate::session_commit_boundary::is_terminal_failure(&message) { boundary.lock().expect("boundary").forget(&session); }
                            else if let Some(model) = registry.entries.get(&session).map(|entry| entry.model_id.clone()).or_else(|| registry.bindings.get(&session).map(|binding| binding.model_id))
                                && boundary.lock().expect("boundary").commit(&session, &message, &model) == crate::session_commit_boundary::AssistantCommitOutcome::Rewritten {
                                    crate::session_entry_annotations::annotate_pending_fork(registry.entries.get_mut(&session), "assistant_rewritten", now);
                                    registry.bindings.forget(&session); registry.bindings.remember_invalidation(&session, Some("assistant_rewritten"));
                            }
                        }
                    },
                    _ => {},
                }
                let invalidation = match event {
                    ExtensionEvent::SessionCompact(SessionCompactEvent::Accepted { .. }) => Some("compaction"),
                    ExtensionEvent::SessionTree { old_leaf_id: Some(_), new_leaf_id: Some(_), .. } => Some("tree_changed"),
                    ExtensionEvent::SessionExtensionsRemoved { .. } => Some("extensions_removed"),
                    ExtensionEvent::MessageEnd { .. } => registry.bindings.invalidation_reason(&session).filter(|reason| *reason == "assistant_rewritten"),
                    _ => None,
                };
                if let Some(reason) = invalidation {
                    let api = ExtensionApi::new(maho_ext_api::LoadedExtension::new("session-registry", cwd, Default::default()), profile, Default::default(), runtime);
                    api.append_entry(crate::session_binding::BINDING_ENTRY_TYPE, Some(serde_json::json!({"schemaVersion":2,"invalidated":true,"reason":reason})))?;
                    if let Some(path) = ctx.session_manager.session_file() { crate::session_binding_store::delete_binding(path).map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))?; }
                }
                Ok(EventResult::None)
            })
        }));
    }
}
