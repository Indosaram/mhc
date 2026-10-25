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
    pub identity: String,
    pub store_path: PathBuf,
    pub policy_digest: String,
    pub provenance_digest: String,
    pub initial: ProjectMcpMaterialization,
    name: String,
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
fn decisions() -> &'static Mutex<BTreeMap<String, Arc<tokio::sync::Mutex<()>>>> {
    static DECISIONS: std::sync::OnceLock<Mutex<BTreeMap<String, Arc<tokio::sync::Mutex<()>>>>> =
        std::sync::OnceLock::new();
    DECISIONS.get_or_init(Default::default)
}
fn decision_lock(path: &Path, identity: &str) -> Arc<tokio::sync::Mutex<()>> {
    decisions()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .entry(format!("{}\0{identity}", path.display()))
        .or_default()
        .clone()
}
fn policies() -> &'static Mutex<BTreeMap<String, Arc<ProjectMcpAuthority>>> {
    static POLICIES: std::sync::OnceLock<Mutex<BTreeMap<String, Arc<ProjectMcpAuthority>>>> =
        std::sync::OnceLock::new();
    POLICIES.get_or_init(Default::default)
}
pub fn get_project_mcp_authority(config: &McpServerConfig) -> Option<Arc<ProjectMcpAuthority>> {
    let digest = crate::config::hash_config(config).ok()?;
    policies()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&digest)
        .cloned()
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
fn identity_for(
    name: &str,
    source: &Path,
    raw: &serde_json::Value,
    cwd: &Path,
) -> Result<String, McpError> {
    let canonical = |path: &Path| {
        std::fs::canonicalize(path)
            .map_err(|error| McpError::new(McpErrorKind::Connect, error.to_string()))
    };
    Ok(declaration_digest(
        &serde_json::json!({"sourcePath":canonical(source)?,"sourceKind":if source.file_name().is_some_and(|name|name=="mcp.json"){"sidecar"}else{"frontmatter"},"projectRoot":canonical(&skill_project_root(cwd))?,"serverName":name,"raw":raw,"cwdBase":canonical(cwd)?}),
    ))
}
impl ProjectMcpAuthority {
    pub fn started_grant_names(&self) -> Vec<String> {
        self.started_names
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
    pub async fn resolve(&self) -> Result<ProjectMcpMaterialization, McpError> {
        let latest = current_raw(&self.name, &self.source_path)?;
        if identity_for(&self.name, &self.source_path, &latest, &self.cwd)? != self.identity {
            return Err(McpError::new(
                McpErrorKind::Connect,
                "Project MCP declaration changed; reload its skills",
            ));
        }
        let decision = decision_lock(&self.store_path, &self.identity);
        let _decision = decision.lock().await;
        let mut names = effective_grant_names(&read(&self.store_path), &self.identity);
        if !self.first.swap(false, std::sync::atomic::Ordering::AcqRel) {
            let policy = crate::project_env::project_mcp_environment(
                &self.raw,
                self.trusted,
                &names,
                Some(&self.explicit),
                &self.ambient,
            )
            .map_err(|error| McpError::new(McpErrorKind::Connect, error.to_string()))?;
            if !policy.withheld.is_empty()
                && let Some(ui) = &self.ui
            {
                let choices = vec!["Allow".into(), "Deny".into()];
                if ui
                    .select(
                        &format!(
                            "Project MCP server {} wants: {}",
                            self.name,
                            policy.withheld.join(", ")
                        ),
                        &choices,
                        Default::default(),
                    )
                    .await
                    .as_deref()
                    == Some("Allow")
                {
                    if identity_for(
                        &self.name,
                        &self.source_path,
                        &current_raw(&self.name, &self.source_path)?,
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
                            project: self.cwd.display().to_string(),
                            skill: String::new(),
                            server: self.name.clone(),
                        },
                        &policy.withheld,
                    ) {
                        Ok(snapshot) => names = effective_grant_names(&snapshot, &self.identity),
                        Err(error) => ui.notify(
                            &format!("Could not save MCP environment grant: {error}"),
                            maho_ext_api::NotificationType::Error,
                        ),
                    }
                }
            }
        }
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
        let wire = serde_json::from_value(policy.declaration)
            .map_err(|error| McpError::new(McpErrorKind::Connect, error.to_string()))?;
        let mut server =
            crate::config::resolve_skill_mcp_server(&self.name, wire, &self.source_path)
                .map_err(|error| McpError::new(McpErrorKind::Connect, error.to_string()))?;
        let mut config = server.config.take().expect("normalized server");
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
pub async fn prepare_project_skill_server(
    name: &str,
    original: &crate::skills::SkillServerDecl,
    cwd: &Path,
    agent_dir: &Path,
    trusted: bool,
    explicit: &BTreeMap<String, String>,
    ui: Option<Arc<dyn maho_ext_api::ExtensionUi>>,
) -> Result<ResolvedMcpServer, McpError> {
    let raw = current_raw(name, &original.source_path)?;
    let identity = identity_for(name, &original.source_path, &raw, cwd)?;
    let store_path = resolve_grants_store_path(Some(agent_dir));
    let ambient = std::env::vars().collect();
    let decision = decision_lock(&store_path, &identity);
    let _decision = decision.lock().await;
    let mut names = effective_grant_names(&read(&store_path), &identity);
    let policy = crate::project_env::project_mcp_environment(
        &raw,
        trusted,
        &names,
        Some(explicit),
        &ambient,
    )
    .map_err(|error| McpError::new(McpErrorKind::Connect, error.to_string()))?;
    if !policy.withheld.is_empty()
        && let Some(ui) = &ui
    {
        let choices = vec!["Allow".into(), "Deny".into()];
        if ui
            .select(
                &format!(
                    "Project MCP server {name} wants: {}",
                    policy.withheld.join(", ")
                ),
                &choices,
                Default::default(),
            )
            .await
            .as_deref()
            == Some("Allow")
            && identity_for(
                name,
                &original.source_path,
                &current_raw(name, &original.source_path)?,
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
                    project: cwd.display().to_string(),
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
    let placeholder = ProjectMcpMaterialization {
        config: crate::config::normalize_server(
            serde_json::from_value(raw.clone())
                .map_err(|error| McpError::new(McpErrorKind::Connect, error.to_string()))?,
        ),
        env: BTreeMap::new(),
        granted_names: vec![],
    };
    let mut authority = ProjectMcpAuthority {
        identity,
        store_path,
        policy_digest: declaration_digest(
            &serde_json::json!({"names":names,"generation":read(&resolve_grants_store_path(Some(agent_dir))).generation}),
        ),
        provenance_digest: declaration_digest(&serde_json::json!({"source":original.source_path})),
        initial: placeholder,
        name: name.into(),
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
    // Keep policy identity in the private connection config so identical commands from
    // separate declarations cannot accidentally share authorization. resolve() strips it.
    config.env.get_or_insert_with(BTreeMap::new).insert(
        "__MHC_PROJECT_POLICY_IDENTITY".into(),
        authority.identity.clone(),
    );
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
        config: Some(config),
    };
    policies()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(hash, Arc::new(authority));
    Ok(server)
}
