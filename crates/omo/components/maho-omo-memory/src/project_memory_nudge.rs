//! Project-scoped memory nudge (#10044).
//!
//! Nudges the model with project-scoped memory when touching files in child projects.

pub type ResolveContextCallback = Arc<dyn Fn(&str) -> Option<MemoryIdentityContext> + Send + Sync>;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::context::MemoryIdentityContext;
use crate::project_memory_grants::{grant_project_memory_read, revoke_project_memory_reads};
use crate::project_recall_seats::{ProjectRecallCorpus, parse_project_memory_path};
use memory_core::recall::RecallDocument;

pub const PROJECT_MEMORY_NUDGE_MAX_NOTES: usize = 30;
pub const PROJECT_MEMORY_RECALL_MAX_PROJECTS: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProjectMemorySwitches {
    pub nudge: bool,
    pub recall: bool,
}

impl Default for ProjectMemorySwitches {
    fn default() -> Self {
        Self {
            nudge: true,
            recall: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TouchedProject {
    pub project_dir: PathBuf,
    pub identity_id: String,
    pub repo_path: PathBuf,
}

#[derive(Default)]
struct SessionScope {
    touched: Vec<TouchedProject>,
    touched_paths: BTreeMap<String, String>,
    nudged: BTreeSet<String>,
    resolver: Option<Arc<memory_core::identity::project_scope::ProjectIdentityResolver>>,
    key: String,
}

pub struct ProjectMemoryScope {
    resolve_context: ResolveContextCallback,
    switches: Arc<dyn Fn() -> ProjectMemorySwitches + Send + Sync>,
    sessions: Mutex<BTreeMap<String, SessionScope>>,
}

impl ProjectMemoryScope {
    pub fn new(
        resolve_context: ResolveContextCallback,
        switches: Arc<dyn Fn() -> ProjectMemorySwitches + Send + Sync>,
    ) -> Self {
        Self {
            resolve_context,
            switches,
            sessions: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn recall_corpora(
        &self,
        session: &str,
        cache: &mut memory_core::recall::RecallCorpusCache,
    ) -> Vec<ProjectRecallCorpus> {
        let sessions = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(scope) = sessions.get(session) else {
            return Vec::new();
        };
        if scope.touched.is_empty() || !(self.switches)().recall {
            return Vec::new();
        }
        scope
            .touched
            .iter()
            .take(PROJECT_MEMORY_RECALL_MAX_PROJECTS)
            .filter_map(|project| {
                let repo =
                    memory_core::git::GitMemoryRepo::open(&project.repo_path, &project.identity_id)
                        .ok()?;
                let corpus = cache.load(&repo).ok()?;
                Some(ProjectRecallCorpus {
                    identity_id: project.identity_id.clone(),
                    documents: corpus.documents.clone(),
                    touched_path: scope.touched_paths.get(&project.identity_id).cloned(),
                })
            })
            .collect()
    }
    pub fn register(
        self: &Arc<Self>,
        api: &mut maho_ext_api::ExtensionApi,
        send: Arc<
            dyn Fn(crate::kibitzer_delivery::KibitzerSteerMessage) -> Result<(), String>
                + Send
                + Sync,
        >,
    ) {
        let scope = self.clone();
        api.on(maho_ext_api::EventKind::ToolResult,Arc::new(move |event,ctx| {
            let scope=scope.clone(); let send=send.clone(); let session=ctx.session_manager.session_id().to_owned(); let cwd=ctx.cwd.clone();
            let path=match event {maho_ext_api::ExtensionEvent::ToolResult(result) if !result.is_error && matches!(result.tool_name.trim().to_lowercase().as_str(),"read"|"edit"|"write")=>["path","file_path","filePath"].iter().find_map(|key|result.input.get(*key).and_then(serde_json::Value::as_str)).map(PathBuf::from),_=>None};
            Box::pin(async move {
                let switches=(scope.switches)();
                if !(switches.nudge || switches.recall) {return Ok(maho_ext_api::EventResult::None)}
                let (Some(path),Some(context))=(path,(scope.resolve_context)(&session)) else {return Ok(maho_ext_api::EventResult::None)};
                let bound=context.project_scope.as_ref();
                let key=format!("{}\0{}\0{:?}",cwd.display(),context.identity,bound);
                let resolver={
                    let mut sessions=scope.sessions.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    let state=sessions.entry(session.clone()).or_default();
                    if state.key!=key {
                        revoke_project_memory_reads(&context); *state=SessionScope::default(); state.key=key;
                        state.resolver=Some(Arc::new(memory_core::identity::project_scope::create_project_identity_resolver(memory_core::identity::project_scope::ProjectIdentityResolverOptions {
                            session_cwd:cwd.clone(),memory_root:bound.map(|value|value.memory_root.clone()),bound_id:Some(context.identity.clone()),
                            auto_identity:if bound.is_some_and(|value|value.auto_identity=="git") {memory_core::identity::resolve::AutoIdentityMode::Git} else {memory_core::identity::resolve::AutoIdentityMode::Path},..Default::default()
                        })));
                    }
                    state.resolver.as_ref().expect("session resolver installed").clone()
                };
                if let Some(project)=resolver.for_path(&path).await {
                    let relative=cwd.join(&path).strip_prefix(&project.project_dir).ok().map(|path|path.to_string_lossy().into_owned());
                    scope.record_touch(&session,project.project_dir.clone(),project.identity.id.clone(),project.identity.paths.repo.clone(),relative);
                    let fresh={let mut sessions=scope.sessions.lock().unwrap_or_else(std::sync::PoisonError::into_inner); sessions.get_mut(&session).expect("touched scope").nudged.insert(project.identity.id.clone())};
                    if switches.nudge && fresh {
                        let repo=memory_core::git::GitMemoryRepo::open(&project.identity.paths.repo,&project.identity.id).map_err(|error|maho_ext_api::ExtensionFailure::new(error.to_string()))?;
                        let corpus=memory_core::recall::RecallCorpusCache::new(Default::default()).load(&repo).map_err(|error|maho_ext_api::ExtensionFailure::new(error.to_string()))?;
                        let content=format_project_memory_nudge(&project.project_dir.to_string_lossy(),&project.identity.paths.repo.to_string_lossy(),&corpus.documents);
                        send(crate::kibitzer_delivery::KibitzerSteerMessage {custom_type:"omo-memory:project".into(),content:memory_core::sync::redact_secret_like_material(&content),display:false}).map_err(maho_ext_api::ExtensionFailure::new)?;
                    }
                }
                Ok(maho_ext_api::EventResult::None)
            })
        }));
        let scope = self.clone();
        api.on(
            maho_ext_api::EventKind::SessionCompact,
            Arc::new(move |event, ctx| {
                if matches!(
                    event,
                    maho_ext_api::ExtensionEvent::SessionCompact(
                        maho_ext_api::SessionCompactEvent::Accepted { .. }
                    )
                )
                    && let Some(state) = scope
                        .sessions
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .get_mut(ctx.session_manager.session_id())
                    {
                        state.nudged.clear();
                    }
                Box::pin(async { Ok(maho_ext_api::EventResult::None) })
            }),
        );
        let scope = self.clone();
        api.on(
            maho_ext_api::EventKind::SessionShutdown,
            Arc::new(move |_, ctx| {
                scope.on_session_shutdown(ctx.session_manager.session_id());
                Box::pin(async { Ok(maho_ext_api::EventResult::None) })
            }),
        );
    }

    pub fn touched_most_recent_first(&self, session_id: &str) -> Vec<TouchedProject> {
        let sessions = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        sessions
            .get(session_id)
            .map(|s| s.touched.clone())
            .unwrap_or_default()
    }

    pub fn source_for(&self, session_id: &str, path: &str) -> Option<String> {
        let (identity_id, subpath) = parse_project_memory_path(path)?;
        if Path::new(&subpath)
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
        {
            return None;
        }
        let sessions = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let scope = sessions.get(session_id)?;
        let project = scope
            .touched
            .iter()
            .find(|p| p.identity_id == identity_id)?;
        Some(
            project
                .repo_path
                .join(subpath)
                .to_string_lossy()
                .to_string(),
        )
    }

    pub fn record_touch(
        &self,
        session_id: &str,
        project_dir: PathBuf,
        identity_id: String,
        repo_path: PathBuf,
        touched_rel: Option<String>,
    ) {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let scope = sessions.entry(session_id.to_string()).or_default();
        if let Some(idx) = scope
            .touched
            .iter()
            .position(|p| p.identity_id == identity_id)
        {
            scope.touched.remove(idx);
        }
        scope.touched.insert(
            0,
            TouchedProject {
                project_dir,
                identity_id: identity_id.clone(),
                repo_path: repo_path.clone(),
            },
        );
        if let Some(rel) = touched_rel {
            scope.touched_paths.insert(identity_id, rel);
        }

        if let Some(ctx) = (self.resolve_context)(session_id) {
            grant_project_memory_read(&ctx, &repo_path);
        }
    }

    pub fn on_session_shutdown(&self, session_id: &str) {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        sessions.remove(session_id);
        if let Some(ctx) = (self.resolve_context)(session_id) {
            revoke_project_memory_reads(&ctx);
        }
    }
}

pub fn format_project_memory_nudge(
    project_dir: &str,
    repo_path: &str,
    notes: &[RecallDocument],
) -> String {
    let limit = notes.len().min(PROJECT_MEMORY_NUDGE_MAX_NOTES);
    let shown = &notes[..limit];
    let mut lines = vec![
        String::new(),
        format!("[Project memory: {project_dir}]"),
        "This project keeps its own memory from earlier sessions. Consult it actively: read any note below that bears on the work before relying on assumptions.".to_string(),
        format!("Notes live under {repo_path}; open one with the read tool at that directory plus the path shown."),
    ];
    for note in shown {
        lines.push(format!("- {}: {}", note.path, note.description));
    }
    if notes.len() > limit {
        lines.push(format!("- +{} more", notes.len() - limit));
    }
    lines.join("\n")
}

pub fn with_project_documents(
    own: &[RecallDocument],
    projects: &[RecallDocument],
) -> Vec<RecallDocument> {
    if projects.is_empty() {
        return own.to_vec();
    }
    let mut merged = own.to_vec();
    merged.extend_from_slice(projects);
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn touched_projects_grant_only_notes_and_shutdown_revokes_sources() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("project-repo");
        std::fs::create_dir_all(&repo).unwrap();
        let paths = memory_core::identity::layout::build_identity_paths(root.path(), "bound");
        let context = MemoryIdentityContext::new(
            "bound".into(),
            paths,
            crate::binding::MemorySessionBinding {
                identity: "bound".into(),
                repo_path_hash: "hash".into(),
                bound_at: 0.0,
            },
        );
        let resolved = context.clone();
        let scope = ProjectMemoryScope::new(
            Arc::new(move |_| Some(resolved.clone())),
            Arc::new(ProjectMemorySwitches::default),
        );
        scope.record_touch(
            "session",
            root.path().to_owned(),
            "child".into(),
            repo.clone(),
            Some("src/main.rs".into()),
        );
        assert_eq!(
            scope.source_for("session", "@project/child/notes/a.md"),
            Some(repo.join("notes/a.md").to_string_lossy().into_owned())
        );
        assert!(
            crate::project_memory_grants::is_granted_project_memory_read(
                &context,
                &repo.join("notes/a.md")
            )
        );
        assert!(
            !crate::project_memory_grants::is_granted_project_memory_read(
                &context,
                &repo.join("system/persona.md")
            )
        );
        assert!(
            scope
                .source_for("session", "@project/child/../secret")
                .is_none()
        );
        scope.on_session_shutdown("session");
        assert!(
            scope
                .source_for("session", "@project/child/notes/a.md")
                .is_none()
        );
        assert!(
            !crate::project_memory_grants::is_granted_project_memory_read(
                &context,
                &repo.join("notes/a.md")
            )
        );
    }

    #[test]
    fn test_format_project_memory_nudge() {
        let notes = vec![RecallDocument {
            path: "reference/deploy.md".to_string(),
            description: "how the child deploys".to_string(),
            body: "body".to_string(),
        }];
        let nudge = format_project_memory_nudge("/workspace/child", "/memory/child/repo", &notes);
        assert!(nudge.contains("[Project memory: /workspace/child]"));
        assert!(nudge.contains("reference/deploy.md: how the child deploys"));
        assert!(nudge.contains("Notes live under /memory/child/repo"));
    }

    #[test]
    fn test_with_project_documents_merging() {
        let own = vec![RecallDocument {
            path: "own.md".to_string(),
            description: "own".to_string(),
            body: "own".to_string(),
        }];
        let proj = vec![RecallDocument {
            path: "@project/child/proj.md".to_string(),
            description: "proj".to_string(),
            body: "proj".to_string(),
        }];
        let merged = with_project_documents(&own, &proj);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].path, "own.md");
        assert_eq!(merged[1].path, "@project/child/proj.md");
    }
}
