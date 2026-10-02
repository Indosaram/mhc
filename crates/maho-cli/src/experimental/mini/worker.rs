use maho_agent::harness::{context::Context, session::{jsonl::{JsonlSessionRepo, JsonlSessionCreateOptions}, types::Session}};
pub fn system_prompt(cwd: &str) -> String {
    ["You are a coding agent working in a terminal.".to_owned(), format!("Working directory: {cwd}"), "Use the read, write, edit, and bash tools to inspect and change files.".to_owned(), "Keep answers short and technical.".to_owned()].join("\n")
}
pub async fn open_session(repo: &JsonlSessionRepo, session_id: Option<&str>, cwd: &str, context: &Context) -> Result<Box<dyn Session>, String> {
    match session_id {
        None => repo.create(JsonlSessionCreateOptions::new(cwd), context).await.map_err(|error| error.to_string()),
        Some(id) => {
            let metadata = repo.list(None, context).await.map_err(|error| error.to_string())?.into_iter().find(|metadata| metadata.id == id).ok_or_else(|| format!("Unknown session: {id}"))?;
            repo.open(metadata, context).await.map_err(|error| error.to_string())
        }
    }
}
