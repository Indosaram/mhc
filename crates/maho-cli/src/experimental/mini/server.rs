use std::sync::Arc;
use maho_agent::harness::{context::BACKGROUND_CONTEXT, env::nodejs::NodeExecutionEnv, session::jsonl::{JsonlSessionRepo, JsonlSessionRepoOptions}, types::FileSystem};
use super::shared::protocol::SessionSummary;
pub async fn list_sessions(sessions_root: &str, cwd: &str) -> Result<Vec<SessionSummary>, String> {
    let env = Arc::new(NodeExecutionEnv::new(cwd));
    let repo = JsonlSessionRepo::new(JsonlSessionRepoOptions { file_system: env.clone(), sessions_root: sessions_root.to_owned(), now: None });
    let result = repo.list(None, &BACKGROUND_CONTEXT).await.map(|sessions| sessions.into_iter().map(|metadata| SessionSummary { id: metadata.id, path: metadata.path, cwd: metadata.cwd, created_at: metadata.created_at as f64 }).collect()).map_err(|error| error.to_string());
    repo.close(&BACKGROUND_CONTEXT).await;
    env.cleanup(&BACKGROUND_CONTEXT).await;
    result
}
