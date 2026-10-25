use super::{
    env_grant_store::{GrantStore, create_file_grant_store},
    environment_grant_policy::{EnvironmentGrantPolicy, Notice},
    project_env_policy::policy_hash,
    types::{Environment, McpClient, SharedState, SkillMcpClientInfo, SkillMcpServerContext},
};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc, time::Duration};
#[derive(Default)]
pub struct SkillMcpClientOptions {
    pub cdp_url: Option<String>,
    pub env: Environment,
}
pub fn build_skill_mcp_client_key(
    info: &SkillMcpClientInfo,
    options: Option<&SkillMcpClientOptions>,
) -> String {
    let mut key = format!(
        "{}:{}:{}",
        info.session_id, info.skill_name, info.server_name
    );
    if let Some(provenance) = &info.provenance {
        key.push_str(&format!(
            "::provenance={}",
            policy_hash(&serde_json::to_value(provenance).expect("provenance serializes"))
        ));
    }
    if let Some(cdp) = options
        .and_then(|o| o.cdp_url.as_deref())
        .filter(|s| !s.is_empty())
    {
        key.push_str(&format!("::cdp={cdp}"));
    }
    key
}
pub struct SkillMcpManager {
    pub state: SharedState,
    pub environment_policy: Arc<EnvironmentGrantPolicy>,
    cleanup: tokio::task::JoinHandle<()>,
    process_cleanup: tokio::task::JoinHandle<()>,
}
impl SkillMcpManager {
    pub fn new(
        config_dir: Option<&Path>,
        store: Option<Arc<dyn GrantStore>>,
        notify: Option<Notice>,
    ) -> Self {
        let state: SharedState = Arc::new(tokio::sync::Mutex::new(Default::default()));
        let weak = Arc::downgrade(&state);
        let policy = EnvironmentGrantPolicy::new(
            store.unwrap_or_else(|| create_file_grant_store(config_dir)),
            notify,
            Arc::new(move |identity| {
                let state = weak.upgrade();
                Box::pin(async move {
                    if let Some(state) = state {
                        let keys = {
                            let mut locked = state.lock().await;
                            locked.pending_connections.retain(|key, _| {
                                !key.contains(&format!("::identity={identity}::"))
                            });
                            locked
                                .clients
                                .iter()
                                .filter(|(_, c)| c.identity.as_deref() == Some(&identity))
                                .map(|(key, _)| key.clone())
                                .collect::<Vec<_>>()
                        };
                        for key in keys {
                            super::cleanup::force_reconnect(&state, &key).await;
                        }
                    }
                })
            }),
        );
        let cleanup = super::cleanup::start_cleanup_timer(&state);
        let process_cleanup = super::cleanup::register_process_cleanup(&state, &policy);
        Self {
            state,
            environment_policy: policy,
            cleanup,
            process_cleanup,
        }
    }
    pub async fn invalidate_identity(&self, identity: &str) {
        self.environment_policy.invalidate_identity(identity).await;
        let keys = {
            let mut state = self.state.lock().await;
            let marker = format!("::identity={identity}::");
            state
                .pending_connections
                .retain(|key, _| !key.contains(&marker));
            state
                .clients
                .keys()
                .filter(|key| key.contains(&marker))
                .cloned()
                .collect::<Vec<_>>()
        };
        for key in keys {
            super::cleanup::force_reconnect(&self.state, &key).await;
        }
    }
    pub async fn grant_environment(
        &self,
        id: &str,
        names: Option<&[String]>,
    ) -> super::Result<String> {
        self.environment_policy.grant(id, names).await
    }
    pub async fn get_environment_notice(&self, info: &SkillMcpClientInfo) -> Option<String> {
        self.environment_policy.notice(info).await
    }
    pub async fn get_or_create_client(
        &self,
        info: &SkillMcpClientInfo,
        config: &Value,
        options: Option<&SkillMcpClientOptions>,
    ) -> super::Result<Arc<McpClient>> {
        let default = SkillMcpClientOptions::default();
        let env = options.unwrap_or(&default);
        super::connection::get_or_create_client(
            &self.state,
            &self.environment_policy,
            build_skill_mcp_client_key(info, options),
            info,
            config,
            &env.env,
            env.cdp_url.as_deref(),
        )
        .await
    }
    pub async fn disconnect_session(&self, session: &str) {
        super::cleanup::disconnect_session(&self.state, session).await;
    }
    pub async fn disconnect_all(&self) {
        super::cleanup::disconnect_all(&self.state, &self.environment_policy).await;
    }
    pub async fn list_tools(
        &self,
        info: &SkillMcpClientInfo,
        context: &SkillMcpServerContext,
        options: Option<&SkillMcpClientOptions>,
    ) -> super::Result<Vec<Value>> {
        self.list(info, context, options, "tools/list", "tools")
            .await
    }
    pub async fn list_resources(
        &self,
        info: &SkillMcpClientInfo,
        context: &SkillMcpServerContext,
        options: Option<&SkillMcpClientOptions>,
    ) -> super::Result<Vec<Value>> {
        self.list(info, context, options, "resources/list", "resources")
            .await
    }
    pub async fn list_prompts(
        &self,
        info: &SkillMcpClientInfo,
        context: &SkillMcpServerContext,
        options: Option<&SkillMcpClientOptions>,
    ) -> super::Result<Vec<Value>> {
        self.list(info, context, options, "prompts/list", "prompts")
            .await
    }
    async fn list(
        &self,
        info: &SkillMcpClientInfo,
        context: &SkillMcpServerContext,
        options: Option<&SkillMcpClientOptions>,
        method: &str,
        field: &str,
    ) -> super::Result<Vec<Value>> {
        let client = self
            .get_or_create_client_with_retry(info, &context.config, options)
            .await?;
        collect_pages(|cursor| {
            let client = client.clone();
            async move {
                let result = client
                    .request(
                        method,
                        cursor.map(|c| json!({"cursor":c})).unwrap_or(json!({})),
                        Duration::from_secs(30),
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                Ok((
                    result[field].as_array().cloned().unwrap_or_default(),
                    result["nextCursor"].as_str().map(str::to_owned),
                ))
            }
        })
        .await
    }
    pub async fn call_tool(
        &self,
        info: &SkillMcpClientInfo,
        context: &SkillMcpServerContext,
        name: &str,
        args: Value,
        options: Option<&SkillMcpClientOptions>,
    ) -> super::Result<Value> {
        self.with_operation_retry(
            info,
            &context.config,
            options,
            "tools/call",
            json!({"name":name,"arguments":args}),
        )
        .await
    }
    pub async fn read_resource(
        &self,
        info: &SkillMcpClientInfo,
        context: &SkillMcpServerContext,
        uri: &str,
        options: Option<&SkillMcpClientOptions>,
    ) -> super::Result<Value> {
        self.with_operation_retry(
            info,
            &context.config,
            options,
            "resources/read",
            json!({"uri":uri}),
        )
        .await
        .map(|v| v["contents"].clone())
    }
    pub async fn get_prompt(
        &self,
        info: &SkillMcpClientInfo,
        context: &SkillMcpServerContext,
        name: &str,
        args: Value,
        options: Option<&SkillMcpClientOptions>,
    ) -> super::Result<Value> {
        self.with_operation_retry(
            info,
            &context.config,
            options,
            "prompts/get",
            json!({"name":name,"arguments":args}),
        )
        .await
        .map(|v| v["messages"].clone())
    }
    async fn with_operation_retry(
        &self,
        info: &SkillMcpClientInfo,
        config: &Value,
        options: Option<&SkillMcpClientOptions>,
        method: &str,
        params: Value,
    ) -> super::Result<Value> {
        for attempt in 1..=3 {
            let result = async {
                let client = self
                    .get_or_create_client_with_retry(info, config, options)
                    .await?;
                client
                    .request(method, params.clone(), Duration::from_secs(30))
                    .await
                    .map_err(|error| error.to_string())
            }
            .await;
            match result {
                Ok(value) => return Ok(value),
                Err(error) => {
                    if !error.to_lowercase().contains("not connected") {
                        return Err(error);
                    }
                    if attempt == 3 {
                        return Err(format!("Failed after 3 reconnection attempts: {error}"));
                    }
                    super::cleanup::force_reconnect(
                        &self.state,
                        &build_skill_mcp_client_key(info, options),
                    )
                    .await;
                }
            }
        }
        unreachable!("bounded retry returns on final attempt")
    }
    async fn get_or_create_client_with_retry(
        &self,
        info: &SkillMcpClientInfo,
        config: &Value,
        options: Option<&SkillMcpClientOptions>,
    ) -> super::Result<Arc<McpClient>> {
        let default = SkillMcpClientOptions::default();
        let options_value = options.unwrap_or(&default);
        super::connection::get_or_create_client_with_retry_impl(
            &self.state,
            &self.environment_policy,
            build_skill_mcp_client_key(info, options),
            info,
            config,
            &options_value.env,
            options_value.cdp_url.as_deref(),
        )
        .await
    }
    pub async fn get_connected_servers(&self) -> Vec<String> {
        self.state.lock().await.clients.keys().cloned().collect()
    }
    pub async fn is_connected(&self, info: &SkillMcpClientInfo) -> bool {
        let base = build_skill_mcp_client_key(info, None);
        self.state
            .lock()
            .await
            .clients
            .keys()
            .any(|key| key == &base || key.starts_with(&format!("{base}::identity=")))
    }
}
impl Drop for SkillMcpManager {
    fn drop(&mut self) {
        self.cleanup.abort();
        self.process_cleanup.abort();
    }
}
pub async fn collect_pages<T, F, Fut>(mut request: F) -> super::Result<Vec<T>>
where
    F: FnMut(Option<String>) -> Fut,
    Fut: std::future::Future<Output = super::Result<(Vec<T>, Option<String>)>>,
{
    let mut items = Vec::new();
    let mut cursor = None;
    for _ in 0..100 {
        let (page, next) = request(cursor).await?;
        items.extend(page);
        if next.is_none() {
            return Ok(items);
        }
        cursor = next;
    }
    Err("MCP list pagination exceeded 100 pages.".into())
}
