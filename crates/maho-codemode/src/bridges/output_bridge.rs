use maho_ext_api::{ExecuteToolError, ExecuteToolErrorCode, ExecuteToolFuture, ExecuteToolOptions};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
enum OutputFormat { #[default] Raw, Tail }

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OutputArgs {
    ids: Vec<String>,
    #[serde(default)]
    format: OutputFormat,
    offset: Option<usize>,
    limit: Option<usize>,
}

pub trait OutputExecuteTool: Send + Sync {
    fn is_tool_available(&self, _name: &str) -> Option<bool> { None }
    fn execute_tool<'a>(&'a self, name: &'a str, params: Value, options: ExecuteToolOptions) -> ExecuteToolFuture<'a>;
}

#[derive(Debug, PartialEq, Serialize)]
#[serde(untagged)]
pub enum EvalOutputResult { Transcript(String), Transcripts(Vec<String>) }

#[derive(Debug, thiserror::Error)]
pub enum OutputBridgeError {
    #[error("output() received invalid arguments: {0}")]
    Arguments(String),
    #[error("output() unavailable: no \"{0}\" tool is registered in this session")]
    Unavailable(String),
    #[error(transparent)]
    Tool(#[from] ExecuteToolError),
}

pub async fn run_eval_output(args: Value, tool_name: &str, executor: &dyn OutputExecuteTool, options: ExecuteToolOptions) -> Result<EvalOutputResult, OutputBridgeError> {
    let parsed: OutputArgs = serde_json::from_value(args).map_err(|error| OutputBridgeError::Arguments(error.to_string()))?;
    if parsed.ids.is_empty() || parsed.ids.iter().any(String::is_empty) || parsed.offset == Some(0) || parsed.limit == Some(0) {
        return Err(OutputBridgeError::Arguments("invalid value".into()));
    }
    if executor.is_tool_available(tool_name) == Some(false) { return Err(OutputBridgeError::Unavailable(tool_name.into())); }
    let mode = match parsed.format { OutputFormat::Raw => "full", OutputFormat::Tail => "tail" };
    let calls = parsed.ids.iter().map(|id| {
        let params = if id.starts_with("st_") { json!({"task_id":id, "mode":mode}) } else { json!({"name":id, "mode":mode}) };
        executor.execute_tool(tool_name, params, options.clone())
    }).collect::<Vec<_>>();
    // Poll all calls together, preserving Promise.all's input order without spawning
    // detached tasks or requiring a 'static session executor.
    let mut pending = calls.into_iter().map(Some).collect::<Vec<_>>();
    let mut results = vec![None; pending.len()];
    std::future::poll_fn(|context| {
        for (index, call) in pending.iter_mut().enumerate() {
            if let Some(future) = call && let std::task::Poll::Ready(result) = future.as_mut().poll(context) {
                *call = None;
                match result {
                    Ok(result) => results[index] = Some(result),
                    Err(error) => return std::task::Poll::Ready(Err(error)),
                }
            }
        }
        if pending.iter().all(Option::is_none) { std::task::Poll::Ready(Ok(())) } else { std::task::Poll::Pending }
    }).await.map_err(|error| match error.code {
        ExecuteToolErrorCode::UnknownTool | ExecuteToolErrorCode::InactiveTool => OutputBridgeError::Unavailable(tool_name.into()),
        ExecuteToolErrorCode::InvalidParams | ExecuteToolErrorCode::Blocked => OutputBridgeError::Tool(error),
    })?;
    let mut transcripts = Vec::new();
    for result in results.into_iter().flatten() {
        let marshalled = crate::tool::tool_result_marshal::marshal_tool_result(&result);
        let text = marshalled["text"].as_str().unwrap_or("");
        let start = parsed.offset.unwrap_or(1) - 1;
        let lines = text.split_inclusive('\n').map(|line| {
            match line.strip_suffix('\n') {
                Some(line) => line.strip_suffix('\r').unwrap_or(line),
                None => line,
            }
        }).chain(text.ends_with('\n').then_some("")).skip(start);
        transcripts.push(lines.take(parsed.limit.unwrap_or(usize::MAX)).collect::<Vec<_>>().join("\n"));
    }
    if transcripts.len() == 1 {
        Ok(EvalOutputResult::Transcript(transcripts.remove(0)))
    } else {
        Ok(EvalOutputResult::Transcripts(transcripts))
    }
}
