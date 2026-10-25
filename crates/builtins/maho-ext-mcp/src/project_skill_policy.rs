use crate::{
    config_schema::{McpServerConfig, ResolvedMcpServer},
    env_grants_store::*,
    errors::{McpError, McpErrorKind},
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
#[derive(Clone)]
pub struct ProjectMcpMaterialization {
    pub config: McpServerConfig,
    pub env: BTreeMap<String, String>,
    pub granted_names: Vec<String>,
}
pub struct ProjectMcpAuthority {
    pub notices: Arc<crate::policy_notices::PolicyNotices>,
    pub notice_event: Option<Arc<dyn Fn(serde_json::Value) + Send + Sync>>,
    pub identity: String,
    pub store_path: PathBuf,
    pub policy_digest: String,
    pub provenance_digest: String,
    pub initial: ProjectMcpMaterialization,
    name: String,
    project_root: PathBuf,
    provenance: Option<crate::skills::SkillProvenance>,
    allowed_set: Option<crate::skills::SkillAllowedSet>,
    skill_path: PathBuf,
    skill_name: String,
    source_path: PathBuf,
    raw: serde_json::Value,
    cwd: PathBuf,
    trusted: bool,
    explicit: BTreeMap<String, String>,
    ambient: BTreeMap<String, String>,
    ui: Option<Arc<dyn maho_ext_api::ExtensionUi>>,
    first: std::sync::atomic::AtomicBool,
    started_names: Mutex<Vec<String>>,
}
fn decisions() -> &'static Mutex<BTreeMap<String, std::sync::Weak<tokio::sync::Mutex<()>>>> {
    static DECISIONS: std::sync::OnceLock<
        Mutex<BTreeMap<String, std::sync::Weak<tokio::sync::Mutex<()>>>>,
    > = std::sync::OnceLock::new();
    DECISIONS.get_or_init(Default::default)
}
fn decision_lock(path: &Path, identity: &str) -> Arc<tokio::sync::Mutex<()>> {
    let mut decisions = decisions()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    decisions.retain(|_, pending| pending.strong_count() > 0);
    let key = format!("{}\0{identity}", path.display());
    if let Some(pending) = decisions.get(&key).and_then(std::sync::Weak::upgrade) {
        return pending;
    }
    let pending = Arc::new(tokio::sync::Mutex::new(()));
    decisions.insert(key, Arc::downgrade(&pending));
    pending
}
/// Contemporaneous callers share completion, including a denied decision.
async fn join_decision(
    decision: Arc<tokio::sync::Mutex<()>>,
) -> (tokio::sync::OwnedMutexGuard<()>, bool) {
    match decision.clone().try_lock_owned() {
        Ok(guard) => (guard, false),
        Err(_) => (decision.lock_owned().await, true),
    }
}
#[derive(Clone)]
pub struct ProjectPolicyHandle(pub Arc<ProjectMcpAuthority>);
impl std::fmt::Debug for ProjectPolicyHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("ProjectPolicy")
            .field(&self.0.identity)
            .finish()
    }
}
impl PartialEq for ProjectPolicyHandle {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
pub fn get_project_mcp_authority(config: &McpServerConfig) -> Option<Arc<ProjectMcpAuthority>> {
    config
        .project_authority
        .as_ref()
        .map(|handle| handle.0.clone())
}
pub fn effective_grant_names(snapshot: &GrantSnapshot, identity: &str) -> Vec<String> {
    snapshot
        .grants
        .get(identity)
        .map_or_else(Vec::new, |grant| grant.names.clone())
}
fn current_raw(name: &str, path: &Path) -> Result<serde_json::Value, McpError> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| McpError::new(McpErrorKind::Connect, error.to_string()))?;
    let raw: serde_json::Value = if path.file_name().is_some_and(|name| name == "mcp.json") {
        serde_json::from_str(&text)
            .map_err(|error| McpError::new(McpErrorKind::Connect, error.to_string()))?
    } else {
        maho_core::frontmatter::parse_frontmatter(&text)
            .map_err(|error| McpError::new(McpErrorKind::Connect, error.message))?
            .frontmatter
            .get("mcp")
            .cloned()
            .unwrap_or_default()
    };
    raw.get("mcpServers")
        .unwrap_or(&raw)
        .get(name)
        .cloned()
        .ok_or_else(|| {
            McpError::new(
                McpErrorKind::Connect,
                "Project MCP declaration is no longer available; reload its skills",
            )
        })
}
pub fn skill_project_root(cwd: &Path) -> PathBuf {
    for ancestor in cwd.ancestors() {
        if ancestor.join(".git").exists() {
            return ancestor.into();
        }
    }
    cwd.into()
}
fn identity_for_roots(
    name: &str,
    source: &Path,
    raw: &serde_json::Value,
    project_root: &Path,
    cwd: &Path,
) -> Result<String, McpError> {
    let canonical = |path: &Path| {
        std::fs::canonicalize(path)
            .map_err(|error| McpError::new(McpErrorKind::Connect, error.to_string()))
    };
    Ok(declaration_digest(
        &serde_json::json!({"sourcePath":canonical(source)?,"sourceKind":if source.file_name().is_some_and(|name|name=="mcp.json"){"sidecar"}else{"frontmatter"},"projectRoot":canonical(project_root)?,"serverName":name,"raw":raw,"cwdBase":canonical(cwd)?}),
    ))
}
fn current_registration(
    name: &str,
    original: &crate::skills::SkillServerDecl,
) -> Result<crate::skills::SkillServerDecl, McpError> {
    crate::skills::parse_skill_mcp_declarations(&[crate::skills::SkillLike {
        name: original.skill_name.clone(),
        file_path: original.skill_path.clone(),
        base_dir: original
            .skill_path
            .parent()
            .unwrap_or(Path::new("."))
            .into(),
        scope: original.scope.clone(),
        provenance: original.provenance.clone(),
        allowed_set: original.allowed_set.clone(),
    }])
    .servers
    .remove(name)
    .ok_or_else(|| {
        McpError::new(
            McpErrorKind::Connect,
            "Project MCP declaration is no longer available; reload its skills",
        )
    })
}
impl ProjectMcpAuthority {
    pub fn started_grant_names(&self) -> Vec<String> {
        self.started_names
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
    pub async fn resolve(&self) -> Result<ProjectMcpMaterialization, McpError> {
        self.resolve_with_signal(None).await
    }
    pub async fn resolve_with_signal(
        &self,
        signal: Option<maho_ext_api::AbortSignal>,
    ) -> Result<ProjectMcpMaterialization, McpError> {
        let cancelled =
            || McpError::new(McpErrorKind::Connect, "Project MCP authorization cancelled");
        if signal.as_ref().is_some_and(|signal| signal.is_aborted()) {
            return Err(cancelled());
        }
        let registration = current_registration(
            &self.name,
            &crate::skills::SkillServerDecl {
                skill_name: self.skill_name.clone(),
                scope: Some("project".into()),
                provenance: self.provenance.clone(),
                allowed_set: self.allowed_set.clone(),
                skill_path: self.skill_path.clone(),
                raw: self.raw.clone(),
                source_path: self.source_path.clone(),
                include_tools_by_skill: BTreeMap::new(),
            },
        )?;
        let latest = registration.raw;
        if registration.source_path != self.source_path
            || identity_for_roots(
                &self.name,
                &self.source_path,
                &latest,
                &self.project_root,
                &self.cwd,
            )? != self.identity
        {
            return Err(McpError::new(
                McpErrorKind::Connect,
                "Project MCP declaration changed; reload its skills",
            ));
        }
        let decision = decision_lock(&self.store_path, &self.identity);
        let joined = join_decision(decision);
        let (_decision, shared_decision) = if let Some(signal) = &signal {
            tokio::select! { biased; _ = signal.cancelled() => return Err(cancelled()), result = joined => result }
        } else {
            joined.await
        };
        if signal.as_ref().is_some_and(|signal| signal.is_aborted()) {
            return Err(cancelled());
        }
        let names = effective_grant_names(&read(&self.store_path), &self.identity);
        if !self.first.swap(false, std::sync::atomic::Ordering::AcqRel) {
            let policy = crate::project_env::project_mcp_environment(
                &self.raw,
                self.trusted,
                &names,
                Some(&self.explicit),
                &self.ambient,
            )
            .map_err(|error| McpError::new(McpErrorKind::Connect, error.to_string()))?;
            if !shared_decision
                && !policy.withheld.is_empty()
                && let Some(ui) = &self.ui
            {
                let choices = vec!["Allow".into(), "Deny".into()];
                let title = format!(
                    "Project MCP server {} wants: {}",
                    self.name,
                    policy.withheld.join(", ")
                );
                let selected = ui.select(
                    &title,
                    &choices,
                    maho_ext_api::ExtensionUiDialogOptions {
                        signal: signal.clone(),
                        ..Default::default()
                    },
                );
                let answer = if let Some(signal) = &signal {
                    tokio::select! { biased; _ = signal.cancelled() => return Err(cancelled()), answer = selected => answer }
                } else {
                    selected.await
                };
                if signal.as_ref().is_some_and(|signal| signal.is_aborted()) {
                    return Err(cancelled());
                }
                if answer.as_deref() == Some("Allow") {
                    if identity_for_roots(
                        &self.name,
                        &self.source_path,
                        &current_raw(&self.name, &self.source_path)?,
                        &self.project_root,
                        &self.cwd,
                    )? != self.identity
                    {
                        return Err(McpError::new(
                            McpErrorKind::Connect,
                            "Project MCP declaration changed; reload its skills",
                        ));
                    }
                    match add_grant(
                        &self.store_path,
                        &self.identity,
                        GrantLabel {
                            project: self.project_root.display().to_string(),
                            skill: self.skill_name.clone(),
                            server: self.name.clone(),
                        },
                        &policy.withheld,
                    ) {
                        Ok(_) => ui.notify(
                            &format!("Saved MCP environment grant for {}", self.name),
                            maho_ext_api::NotificationType::Info,
                        ),
                        Err(error) => ui.notify(
                            &format!("Could not save MCP environment grant: {error}"),
                            maho_ext_api::NotificationType::Error,
                        ),
                    }
                }
            }
        }
        if identity_for_roots(
            &self.name,
            &self.source_path,
            &current_raw(&self.name, &self.source_path)?,
            &self.project_root,
            &self.cwd,
        )? != self.identity
        {
            return Err(McpError::new(
                McpErrorKind::Connect,
                "Project MCP declaration changed; reload its skills",
            ));
        }
        let names = effective_grant_names(&read(&self.store_path), &self.identity);
        let materialized = self.materialize(&names)?;
        *self
            .started_names
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = names;
        Ok(materialized)
    }
    fn materialize(&self, names: &[String]) -> Result<ProjectMcpMaterialization, McpError> {
        let policy = crate::project_env::project_mcp_environment(
            &self.raw,
            self.trusted,
            names,
            Some(&self.explicit),
            &self.ambient,
        )
        .map_err(|error| McpError::new(McpErrorKind::Connect, error.to_string()))?;
        let (event, warning) = self.notices.notice(
            &self.identity,
            &self.name,
            &self.skill_name,
            self.trusted,
            &policy.withheld,
        );
        if let Some(emit) = &self.notice_event {
            emit(event);
        }
        if let Some(warning) = warning {
            maho_ext_api::diagnostic(&warning);
        }
        let wire = serde_json::from_value(policy.declaration)
            .map_err(|error| McpError::new(McpErrorKind::Connect, error.to_string()))?;
        let mut server =
            crate::config::resolve_skill_mcp_server(&self.name, wire, &self.source_path)
                .map_err(|error| McpError::new(McpErrorKind::Connect, error.to_string()))?;
        let mut config = server.config.take().expect("normalized server");
        config.direct_tools = Some(crate::config_schema::DirectTools::Patterns(Vec::new()));
        config.exposure = Some(crate::config_schema::Exposure::Search);
        config.env = Some(BTreeMap::new());
        if config.transport == Some(crate::config_schema::Transport::Stdio) {
            config.cwd = Some(
                self.cwd
                    .join(config.cwd.as_deref().unwrap_or("."))
                    .to_string_lossy()
                    .into_owned(),
            );
        }
        Ok(ProjectMcpMaterialization {
            config,
            env: policy.env,
            granted_names: names.to_vec(),
        })
    }
}
pub fn reuse_project_skill_server(
    name: &str,
    original: &crate::skills::SkillServerDecl,
    cwd: &Path,
    agent_dir: &Path,
    existing: &ResolvedMcpServer,
) -> Result<bool, McpError> {
    let Some(authority) = existing.config.as_ref().and_then(get_project_mcp_authority) else {
        return Ok(false);
    };
    let current = current_registration(name, original)?;
    let root = current
        .provenance
        .as_ref()
        .and_then(|provenance| provenance.project_root.clone())
        .unwrap_or_else(|| skill_project_root(cwd));
    let provenance = declaration_digest(
        &serde_json::json!({"provenance":current.provenance,"allowedSet":current.allowed_set.as_ref().map(|set| &set.digest)}),
    );
    if authority.identity
        != identity_for_roots(name, &current.source_path, &current.raw, &root, cwd)?
        || authority.store_path != resolve_grants_store_path(Some(agent_dir))
        || authority.provenance_digest != provenance
    {
        return Ok(false);
    }
    authority.materialize(&effective_grant_names(
        &read(&authority.store_path),
        &authority.identity,
    ))?;
    Ok(true)
}
pub async fn prepare_project_skill_server(
    name: &str,
    original: &crate::skills::SkillServerDecl,
    cwd: &Path,
    agent_dir: &Path,
    trusted: bool,
    explicit: &BTreeMap<String, String>,
    ui: Option<Arc<dyn maho_ext_api::ExtensionUi>>,
) -> Result<ResolvedMcpServer, McpError> {
    prepare_project_skill_server_with_notices(
        name,
        original,
        cwd,
        agent_dir,
        trusted,
        explicit, (ui,
        Arc::default(),
        None),
    )
    .await
}
pub type NoticePresentation = (Option<Arc<dyn maho_ext_api::ExtensionUi>>, Arc<crate::policy_notices::PolicyNotices>, Option<Arc<dyn Fn(serde_json::Value) + Send + Sync>>);
pub async fn prepare_project_skill_server_with_notices(
    name: &str,
    original: &crate::skills::SkillServerDecl,
    cwd: &Path,
    agent_dir: &Path,
    trusted: bool,
    explicit: &BTreeMap<String, String>,
    presentation: NoticePresentation,
) -> Result<ResolvedMcpServer, McpError> {
    let (ui, notices, notice_event) = presentation;
    let original = &current_registration(name, original)?;
    let project_root = original
        .provenance
        .as_ref()
        .and_then(|provenance| provenance.project_root.clone())
        .unwrap_or_else(|| skill_project_root(cwd));
    let raw = original.raw.clone();
    let identity = identity_for_roots(name, &original.source_path, &raw, &project_root, cwd)?;
    let store_path = resolve_grants_store_path(Some(agent_dir));
    let ambient = std::env::vars().collect();
    let decision = decision_lock(&store_path, &identity);
    let (_decision, shared_decision) = join_decision(decision).await;
    let mut names = effective_grant_names(&read(&store_path), &identity);
    let policy = crate::project_env::project_mcp_environment(
        &raw,
        trusted,
        &names,
        Some(explicit),
        &ambient,
    )
    .map_err(|error| McpError::new(McpErrorKind::Connect, error.to_string()))?;
    if !shared_decision && !policy.withheld.is_empty() {
        let (event, warning) = notices.notice(
            &identity,
            name,
            &original.skill_name,
            trusted,
            &policy.withheld,
        );
        if let Some(emit) = &notice_event {
            emit(event);
        }
        if let Some(warning) = warning {
            maho_ext_api::diagnostic(&warning);
        }
    }
    if !shared_decision
        && !policy.withheld.is_empty()
        && let Some(ui) = &ui
    {
        let choices = vec!["Allow".into(), "Deny".into()];
        if ui
            .select(
                &format!(
                    "Project MCP server {name} (skill {}) wants: {}",
                    original.skill_name,
                    policy.withheld.join(", ")
                ),
                &choices,
                Default::default(),
            )
            .await
            .as_deref()
            == Some("Allow")
            && identity_for_roots(
                name,
                &original.source_path,
                &current_raw(name, &original.source_path)?,
                &project_root,
                cwd,
            )? == identity
        {
            let skill = original
                .include_tools_by_skill
                .keys()
                .next()
                .cloned()
                .unwrap_or_default();
            match add_grant(
                &store_path,
                &identity,
                GrantLabel {
                    project: project_root.display().to_string(),
                    skill,
                    server: name.into(),
                },
                &policy.withheld,
            ) {
                Ok(snapshot) => names = effective_grant_names(&snapshot, &identity),
                Err(error) => ui.notify(
                    &format!("Could not save MCP environment grant for {name}: {error}"),
                    maho_ext_api::NotificationType::Error,
                ),
            }
        }
    }
    let latest = current_registration(name, original)?;
    if identity_for_roots(name, &latest.source_path, &latest.raw, &project_root, cwd)? != identity {
        drop(_decision);
        return Box::pin(prepare_project_skill_server_with_notices(
            name,
            &latest,
            cwd,
            agent_dir,
            trusted,
            explicit, (ui,
            notices,
            notice_event),
        ))
        .await;
    }
    let placeholder = ProjectMcpMaterialization {
        config: crate::config::normalize_server(
            serde_json::from_value(raw.clone())
                .map_err(|error| McpError::new(McpErrorKind::Connect, error.to_string()))?,
        ),
        env: BTreeMap::new(),
        granted_names: vec![],
    };
    let mut authority = ProjectMcpAuthority {
        notices,
        notice_event,
        identity,
        store_path,
        policy_digest: declaration_digest(
            &serde_json::json!({"names":names,"generation":read(&resolve_grants_store_path(Some(agent_dir))).generation}),
        ),
        provenance_digest: declaration_digest(
            &serde_json::json!({"provenance":original.provenance,"allowedSet":original.allowed_set.as_ref().map(|set| &set.digest)}),
        ),
        initial: placeholder,
        name: name.into(),
        project_root,
        provenance: original.provenance.clone(),
        allowed_set: original.allowed_set.clone(),
        skill_path: original.skill_path.clone(),
        skill_name: original.skill_name.clone(),
        source_path: original.source_path.clone(),
        raw,
        cwd: cwd.into(),
        trusted,
        explicit: explicit.clone(),
        ambient,
        ui,
        first: std::sync::atomic::AtomicBool::new(true),
        started_names: Mutex::new(names.clone()),
    };
    authority.initial = authority.materialize(&names)?;
    let mut config = authority.initial.config.clone();
    let hash = crate::config::hash_config(&config)
        .map_err(|error| McpError::new(McpErrorKind::Connect, error.to_string()))?;
    let server = ResolvedMcpServer {
        name: name.into(),
        source: crate::config_schema::McpServerSource::Skill,
        source_path: original.source_path.clone(),
        state: if config.enabled == Some(false) {
            crate::config_schema::McpServerState::Disabled
        } else {
            crate::config_schema::McpServerState::Enabled
        },
        transport: config.transport,
        config_hash: Some(declaration_digest(
            &serde_json::json!({"config":hash,"identity":authority.identity,"policy":authority.policy_digest,"provenance":authority.provenance_digest}),
        )),
        config: {
            config.project_authority = Some(ProjectPolicyHandle(Arc::new(authority)));
            Some(config)
        },
    };
    Ok(server)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn concurrent_denial_shares_completion_and_later_attempt_can_retry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("grants.json");
        let pending = decision_lock(&path, "identity");
        let (guard, shared) = join_decision(pending).await;
        assert!(!shared);
        let next = decision_lock(&path, "identity");
        let mut joined = Box::pin(join_decision(next));
        assert!(futures::poll!(joined.as_mut()).is_pending());
        drop(guard);
        let (joined_guard, shared) = joined.await;
        assert!(shared);
        drop(joined_guard);
        let (_, shared) = join_decision(decision_lock(&path, "identity")).await;
        assert!(!shared);
    }
    #[test]
    fn project_root_is_independent_of_cwd_and_raw_edits_change_identity() {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().join("nested");
        std::fs::create_dir(&cwd).unwrap();
        let source = root.path().join("mcp.json");
        std::fs::write(&source, "{}").unwrap();
        let raw = serde_json::json!({"command":"server"});
        let identity = identity_for_roots("probe", &source, &raw, root.path(), &cwd).unwrap();
        assert_eq!(
            identity,
            declaration_digest(&serde_json::json!({
                "sourcePath": std::fs::canonicalize(&source).unwrap(), "sourceKind":"sidecar",
                "projectRoot": std::fs::canonicalize(root.path()).unwrap(), "serverName":"probe",
                "raw":raw, "cwdBase":std::fs::canonicalize(&cwd).unwrap()
            }))
        );
        assert_ne!(
            identity,
            identity_for_roots(
                "probe",
                &source,
                &serde_json::json!({"command":"changed"}),
                root.path(),
                &cwd
            )
            .unwrap()
        );
    }
    #[tokio::test]
    async fn config_clones_own_policy_and_last_drop_retires_it() {
        let root = tempfile::tempdir().unwrap();
        let skill = root.path().join("SKILL.md");
        std::fs::write(
            &skill,
            "---\nname: fixture\nmcp:\n  probe:\n    command: server\n---\n",
        )
        .unwrap();
        let declarations =
            crate::skills::parse_skill_mcp_declarations(&[crate::skills::SkillLike {
                name: "fixture".into(),
                file_path: skill,
                base_dir: root.path().into(),
                scope: Some("project".into()),
                ..Default::default()
            }]);
        let server = prepare_project_skill_server(
            "probe",
            &declarations.servers["probe"],
            root.path(),
            root.path(),
            true,
            &BTreeMap::new(),
            None,
        )
        .await
        .unwrap();
        let config = server.config.as_ref().unwrap();
        let policy = get_project_mcp_authority(config).unwrap();
        let weak = Arc::downgrade(&policy);
        let cloned = config.clone();
        assert!(
            !serde_json::to_string(config)
                .unwrap()
                .contains("project_authority")
        );
        drop(policy);
        drop(server);
        assert!(weak.upgrade().is_some());
        drop(cloned);
        assert!(weak.upgrade().is_none());
    }
    #[tokio::test]
    async fn policy_materialization_emits_withheld_metadata_and_empty_removal() {
        let root = tempfile::tempdir().unwrap();
        let skill = root.path().join("SKILL.md");
        std::fs::write(&skill, "---\nname: fixture\n---\nbody").unwrap();
        std::fs::write(root.path().join("mcp.json"), serde_json::json!({
            "probe": {"command":"server", "args":["${SECNOTICE_TEST_NAME}"]},
            "empty": {"command":"server"},
        }).to_string()).unwrap();
        let declarations = crate::skills::parse_skill_mcp_declarations(&[crate::skills::SkillLike {
            name: "fixture".into(), file_path: skill, base_dir: root.path().into(),
            scope: Some("project".into()), ..Default::default()
        }]);
        let events = Arc::new(Mutex::new(Vec::new()));
        for name in ["probe", "empty"] {
            let sink = events.clone();
            let _server = prepare_project_skill_server_with_notices(
                name, &declarations.servers[name], root.path(), root.path(), false,
                &BTreeMap::new(), (None, Arc::new(Default::default()),
                Some(Arc::new(move |event| sink.lock().unwrap().push(event)))),
            ).await.unwrap();
        }
        let events = events.lock().unwrap();
        let withheld = events.iter().find(|event| event["remove"] == false).unwrap();
        assert_eq!(withheld["category"], "mcp-env-withheld");
        assert!(withheld["path"].is_string());
        let removed = events.iter().find(|event| event["remove"] == true).unwrap();
        assert_eq!(removed["category"], "mcp-env-withheld");
        assert!(removed["path"].is_string());
        assert_eq!(removed["message"], "");
    }

}
