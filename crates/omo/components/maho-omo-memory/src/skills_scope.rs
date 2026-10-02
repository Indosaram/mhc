use std::{path::PathBuf, sync::Arc};
use maho_ext_api::{EventKind, EventResult, ExtensionApi, ResourcesDiscoverResult};
use crate::context::MemoryIdentityContext;

pub const MEMORY_SKILLS_DIRNAME: &str = "skills";
pub type ResolveSkillsContext = Arc<dyn Fn(&str) -> Option<MemoryIdentityContext> + Send + Sync>;

pub fn memory_skills_dir(context: &MemoryIdentityContext) -> PathBuf {
    context.identity_paths.repo.join(MEMORY_SKILLS_DIRNAME)
}

pub fn discover_memory_skills(event: EventKind, session_id: Option<&str>, resolve: &dyn Fn(&str) -> Option<MemoryIdentityContext>) -> Option<ResourcesDiscoverResult> {
    if event != EventKind::ResourcesDiscover { return None; }
    let context = resolve(session_id.filter(|id| !id.is_empty())?)?;
    let directory = memory_skills_dir(&context);
    directory.exists().then(|| ResourcesDiscoverResult { skill_paths: vec![directory.to_string_lossy().into_owned().into()], ..Default::default() })
}

pub fn register_memory_skills_scope(api: &mut ExtensionApi, resolve: ResolveSkillsContext) {
    api.on(EventKind::ResourcesDiscover, Arc::new(move |event, context| {
        let result = discover_memory_skills(event.kind(), Some(context.session_manager.session_id()), resolve.as_ref());
        Box::pin(async move { Ok(result.map_or(EventResult::None, EventResult::ResourcesDiscover)) })
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::binding::MemorySessionBinding;
    fn fixture(root: &std::path::Path, create: bool) -> MemoryIdentityContext {
        let paths = memory_core::identity::layout::build_identity_paths(root, "agent");
        if create { std::fs::create_dir_all(paths.repo.join("skills")).unwrap(); }
        MemoryIdentityContext::new("agent".into(), paths, MemorySessionBinding { identity: "agent".into(), repo_path_hash: "hash".into(), bound_at: 0.0 })
    }
    #[test] fn resolves_repo_skills_directory() { let root = tempfile::tempdir().unwrap(); let context = fixture(root.path(), true); assert_eq!(memory_skills_dir(&context), context.identity_paths.repo.join("skills")); }
    #[test]fn discovery_tracks_directory_appearance(){let root=tempfile::tempdir().unwrap();let context=fixture(root.path(),false);let resolve=|_:&str|Some(context.clone());assert!(discover_memory_skills(EventKind::ResourcesDiscover,Some("session"),&resolve).is_none());std::fs::create_dir_all(memory_skills_dir(&context)).unwrap();assert_eq!(discover_memory_skills(EventKind::ResourcesDiscover,Some("session"),&resolve).unwrap().skill_paths.len(),1);}
    #[test]fn discovery_tracks_directory_removal(){let root=tempfile::tempdir().unwrap();let context=fixture(root.path(),true);let resolve=|_:&str|Some(context.clone());assert!(discover_memory_skills(EventKind::ResourcesDiscover,Some("session"),&resolve).is_some());std::fs::remove_dir(memory_skills_dir(&context)).unwrap();assert!(discover_memory_skills(EventKind::ResourcesDiscover,Some("session"),&resolve).is_none());}
    #[test] fn startup_contributes_bound_path() { let root = tempfile::tempdir().unwrap(); let context = fixture(root.path(), true); let result = discover_memory_skills(EventKind::ResourcesDiscover, Some("session"), &|_| Some(context.clone())).unwrap(); assert_eq!(result.skill_paths[0].path, memory_skills_dir(&context).to_string_lossy()); }
    #[test] fn reload_contributes_bound_path() { let root = tempfile::tempdir().unwrap(); let context = fixture(root.path(), true); assert_eq!(discover_memory_skills(EventKind::ResourcesDiscover, Some("session"), &|_| Some(context.clone())).unwrap().skill_paths.len(), 1); }
    #[test] fn resolves_by_event_session() { let root = tempfile::tempdir().unwrap(); let context = fixture(root.path(), true); let resolve = |id: &str| (id == "bound").then(|| context.clone()); assert!(discover_memory_skills(EventKind::ResourcesDiscover, Some("bound"), &resolve).is_some()); assert!(discover_memory_skills(EventKind::ResourcesDiscover, Some("other"), &resolve).is_none()); }
    #[test] fn unbound_session_passes_through() { assert!(discover_memory_skills(EventKind::ResourcesDiscover, Some("session"), &|_| None).is_none()); }
    #[test] fn missing_directory_passes_through_without_creation() { let root = tempfile::tempdir().unwrap(); let context = fixture(root.path(), false); assert!(discover_memory_skills(EventKind::ResourcesDiscover, Some("session"), &|_| Some(context.clone())).is_none()); assert!(!memory_skills_dir(&context).exists()); }
    #[test] fn unrelated_event_never_resolves_context() { assert!(discover_memory_skills(EventKind::SessionStart, Some("session"), &|_| panic!("must not resolve")).is_none()); }
    #[test] fn absent_or_empty_session_never_resolves_context() { for session in [None, Some("")] { assert!(discover_memory_skills(EventKind::ResourcesDiscover, session, &|_| panic!("must not resolve")).is_none()); } }
    #[test] fn discovery_contributes_only_additional_paths() { let root = tempfile::tempdir().unwrap(); let context = fixture(root.path(), true); let result = discover_memory_skills(EventKind::ResourcesDiscover, Some("session"), &|_| Some(context.clone())).unwrap(); assert!(result.prompt_paths.is_empty() && result.theme_paths.is_empty() && result.hook_paths.is_empty()); assert_eq!(result.skill_paths.len(), 1); }
    #[test] fn repeated_discovery_is_stable() { let root = tempfile::tempdir().unwrap(); let context = fixture(root.path(), true); let resolve = |_: &str| Some(context.clone()); assert_eq!(discover_memory_skills(EventKind::ResourcesDiscover, Some("session"), &resolve), discover_memory_skills(EventKind::ResourcesDiscover, Some("session"), &resolve)); }
    #[test] fn registers_native_resources_handler() { let mut api = ExtensionApi::new(maho_ext_api::LoadedExtension::new("memory", PathBuf::new(), Default::default()), Default::default(), Default::default(), Default::default()); register_memory_skills_scope(&mut api, Arc::new(|_| None)); assert_eq!(api.registered.handlers.len(), 1); assert_eq!(api.registered.handlers[&EventKind::ResourcesDiscover].len(), 1); }
}
