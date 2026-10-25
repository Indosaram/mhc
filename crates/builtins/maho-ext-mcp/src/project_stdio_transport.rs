use crate::{
    errors::{McpError, McpErrorKind},
    project_skill_policy::{ProjectMcpAuthority, ProjectMcpMaterialization},
};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
/// Policy is resolved before every fresh native spawn. The owning ServerConnection
/// checks its generation again after initialize and closes any superseded client.
pub struct ProjectStdioTransport {
    pub authority: Arc<ProjectMcpAuthority>,
    generation: AtomicU64,
}
impl ProjectStdioTransport {
    pub fn new(authority: Arc<ProjectMcpAuthority>) -> Self {
        Self {
            authority,
            generation: AtomicU64::new(0),
        }
    }
    pub fn invalidate(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
    }
    pub async fn start(&self) -> Result<ProjectMcpMaterialization, McpError> {
        self.start_with_signal(None).await
    }
    pub async fn start_with_signal(
        &self,
        signal: Option<maho_ext_api::AbortSignal>,
    ) -> Result<ProjectMcpMaterialization, McpError> {
        let generation = self.generation.load(Ordering::Acquire);
        let resolved = self.authority.resolve_with_signal(signal).await?;
        if self.generation.load(Ordering::Acquire) != generation {
            let mut error = McpError::new(
                McpErrorKind::Connect,
                "Project MCP spawn was invalidated; retry",
            );
            error.phase = Some("create".into());
            error.retriable = true;
            return Err(error);
        }
        Ok(resolved)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn every_start_reloads_grants_and_rejects_changed_declaration() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("mcp.json");
        std::fs::write(
            &source,
            r#"{"mcpServers":{"test":{"command":"server","args":["${TOKEN}"]}}}"#,
        )
        .unwrap();
        let declaration = crate::skills::SkillServerDecl {
            skill_name: "fixture".into(),
            scope: Some("project".into()),
            provenance: None,
            allowed_set: None,
            skill_path: dir.path().join("SKILL.md"),
            raw: serde_json::json!({"command":"server","args":["${TOKEN}"]}),
            source_path: source.clone(),
            include_tools_by_skill: Default::default(),
        };
        let env = std::collections::BTreeMap::from([("TOKEN".into(), "secret".into())]);
        let server = crate::project_skill_policy::prepare_project_skill_server(
            "test",
            &declaration,
            dir.path(),
            dir.path(),
            false,
            &env,
            None,
        )
        .await
        .unwrap();
        let authority =
            crate::project_skill_policy::get_project_mcp_authority(server.config.as_ref().unwrap())
                .unwrap();
        let transport = ProjectStdioTransport::new(authority.clone());
        assert_eq!(
            transport.start().await.unwrap().config.args.unwrap(),
            ["${TOKEN}"]
        );
        crate::env_grants_store::add_grant(
            &authority.store_path,
            &authority.identity,
            crate::env_grants_store::GrantLabel {
                project: "p".into(),
                skill: "s".into(),
                server: "test".into(),
            },
            &["TOKEN".into()],
        )
        .unwrap();
        assert_eq!(
            transport.start().await.unwrap().config.args.unwrap(),
            ["secret"]
        );
        crate::env_grants_store::revoke(&authority.store_path, &authority.identity, None).unwrap();
        assert_eq!(
            transport.start().await.unwrap().config.args.unwrap(),
            ["${TOKEN}"]
        );
        std::fs::write(
            &source,
            r#"{"mcpServers":{"test":{"command":"different"}}}"#,
        )
        .unwrap();
        assert!(transport.start().await.is_err());
    }
}
