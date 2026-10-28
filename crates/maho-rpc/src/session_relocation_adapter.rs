//! Adapter from connection-scoped wire relocation to the integrated core result types.
pub type SendCallback = Arc<dyn Fn(&str, Value) + Send + Sync>;
use serde_json::Value;
use std::sync::Arc;
pub struct SessionRelocationAdapter {
    pub relocation: crate::session_relocation_legacy::RpcSessionRelocation,
    pub session: Arc<dyn Fn() -> Option<maho_core::agent_session::AgentSession> + Send + Sync>,
    pub offerer: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    pub send: SendCallback,
}
#[async_trait::async_trait]
impl maho_core::session_relocation::SessionRelocationHost for SessionRelocationAdapter {
    fn offered(&self) -> bool {
        (self.offerer)().is_some()
    }
    async fn targets(&self) -> maho_core::session_relocation::RelocationTargetsResult {
        let connection = (self.offerer)();
        let result = self
            .relocation
            .targets(connection.as_deref(), |record| {
                if let Some(id) = &connection {
                    (self.send)(id, record);
                }
            })
            .await;
        serde_json::from_value(result).expect("validated relocation targets")
    }
    async fn request(
        &self,
        request: maho_core::session_relocation::RelocationRequest,
    ) -> maho_core::session_relocation::RelocationRequestResult {
        let connection = (self.offerer)();
        let saved = (self.session)().is_some_and(|session| session.session_file().is_some());
        let result = self
            .relocation
            .request(
                &request.target,
                &request.reason,
                connection.as_deref(),
                saved,
                |record| {
                    if let Some(id) = &connection {
                        (self.send)(id, record);
                    }
                },
            )
            .await;
        serde_json::from_value(result).expect("validated relocation request")
    }
}
