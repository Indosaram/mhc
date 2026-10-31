//! Port of senpi `experimental/micro/tools.ts` (pin 903b8336): the mini execution tools (read,
//! bash, edit, write) adapted to Pico3's smaller `ToolDeclaration` contract.
//!
//! Divergence: senpi's invocation forwards `getMemo`/`setMemo` to Pico's `api.memo`. The harness
//! invocation must be `Send + Sync` while Pico's `ToolApi` is a local (`!Send`) borrow, and none of
//! the four adapted tools reads or writes a memo, so the invocation here keeps an in-call memo only.

use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use maho_agent::harness::context::Context;
use maho_agent::harness::pico3::bounded::Retain;
use maho_agent::harness::pico3::types::{
    Error, JsonObject, JsonValue, LocalBoxFuture, OutputLimits, Replay, Result, ToolApi,
    ToolContent, ToolControl, ToolDeclaration, ToolProgress, ToolResult, True,
};
use maho_agent::harness::tools::{
    BashToolOptions, ExecutionToolContext, ReadToolOptions, create_bash_tool, create_edit_tool,
    create_read_tool, create_write_tool,
};
use maho_agent::harness::types::{
    AgentHarnessTool, AgentHarnessToolInvocation, AgentHarnessToolUpdateOptions, ExecutionEnv,
};
use maho_agent::types::AgentToolResult;
use maho_ai::types::{BoxFuture, ContentBlock};

use super::models::ModelToolMetadata;

/// senpi `MicroTools`.
pub struct MicroTools {
    pub declarations: Vec<Rc<dyn maho_agent::harness::pico3::types::AnyToolDeclaration>>,
    pub model_metadata: BTreeMap<String, ModelToolMetadata>,
}

fn limits(retain: Retain) -> OutputLimits {
    OutputLimits {
        max_bytes: Some(128 * 1024),
        max_lines: Some(2500),
        retain: Some(retain),
    }
}

/// senpi `createMicroTools`: adapt the same execution tools mini uses.
pub fn create_micro_tools(env: Arc<dyn ExecutionEnv>) -> std::result::Result<MicroTools, String> {
    let read = create_read_tool::<ExecutionToolContext>(ReadToolOptions::default());
    let bash = create_bash_tool::<ExecutionToolContext>(BashToolOptions::default());
    let edit = create_edit_tool::<ExecutionToolContext>();
    let write = create_write_tool::<ExecutionToolContext>();
    let model_metadata = [&read, &bash, &edit, &write]
        .iter()
        .map(|tool| {
            (
                tool.name().to_owned(),
                ModelToolMetadata {
                    constrained_sampling: tool.tool.constrained_sampling.clone(),
                },
            )
        })
        .collect();
    let declarations: Vec<Rc<dyn maho_agent::harness::pico3::types::AnyToolDeclaration>> = vec![
        Rc::new(adapt_tool(
            read,
            env.clone(),
            Replay::Safe,
            limits(Retain::Head),
        )?),
        Rc::new(adapt_tool(
            bash,
            env.clone(),
            Replay::Unsafe,
            limits(Retain::Tail),
        )?),
        Rc::new(adapt_tool(
            edit,
            env.clone(),
            Replay::Unsafe,
            limits(Retain::Head),
        )?),
        Rc::new(adapt_tool(
            write,
            env,
            Replay::Unsafe,
            limits(Retain::Head),
        )?),
    ];
    Ok(MicroTools {
        declarations,
        model_metadata,
    })
}

/// One adapted harness tool.
pub struct MicroTool {
    tool: AgentHarnessTool<ExecutionToolContext>,
    parameters: JsonObject,
    env: Arc<dyn ExecutionEnv>,
    replay: Replay,
    output: OutputLimits,
}

fn adapt_tool(
    tool: AgentHarnessTool<ExecutionToolContext>,
    env: Arc<dyn ExecutionEnv>,
    replay: Replay,
    output: OutputLimits,
) -> std::result::Result<MicroTool, String> {
    let parameters: JsonObject =
        serde_json::from_value(tool.tool.parameters.clone()).map_err(|error| error.to_string())?;
    Ok(MicroTool {
        tool,
        parameters,
        env,
        replay,
        output,
    })
}

struct LocalInvocation {
    invocation_id: String,
    operation_id: String,
    turn_id: String,
    memo: Mutex<HashMap<String, serde_json::Value>>,
}

impl AgentHarnessToolInvocation for LocalInvocation {
    fn invocation_id(&self) -> &str {
        &self.invocation_id
    }
    fn operation_id(&self) -> &str {
        &self.operation_id
    }
    fn turn_id(&self) -> &str {
        &self.turn_id
    }
    fn get_memo<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Option<serde_json::Value>> {
        let value = self
            .memo
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(name)
            .cloned();
        Box::pin(async move { value })
    }
    fn set_memo<'a>(
        &'a self,
        name: &'a str,
        value: Option<serde_json::Value>,
    ) -> BoxFuture<'a, ()> {
        if let Some(value) = value {
            self.memo
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .insert(name.to_owned(), value);
        }
        Box::pin(async {})
    }
}

/// senpi `jsonValue`: a JSON round trip; `undefined` (here `null`) stays absent.
fn json_value(value: &serde_json::Value) -> Option<JsonValue> {
    if value.is_null() {
        return None;
    }
    serde_json::from_value(value.clone()).ok()
}

fn text_of(content: &[ContentBlock]) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect()
}

fn tool_content(content: Vec<ContentBlock>) -> Vec<ToolContent> {
    content
        .into_iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(ToolContent::Text { text: text.text }),
            ContentBlock::Image(image) => Some(ToolContent::Image {
                data: image.data,
                mime_type: image.mime_type,
            }),
            _ => None,
        })
        .collect()
}

#[derive(Default)]
struct PendingProgress {
    latest: Option<AgentToolResult>,
    checkpoint: bool,
}

impl ToolDeclaration for MicroTool {
    type Args = serde_json::Value;

    fn name(&self) -> &str {
        self.tool.name()
    }
    fn description(&self) -> &str {
        &self.tool.tool.description
    }
    fn parameters(&self) -> &JsonObject {
        &self.parameters
    }
    fn replay(&self) -> Replay {
        self.replay
    }
    fn output(&self) -> OutputLimits {
        self.output.clone()
    }

    fn execute<'a>(
        &'a self,
        args: serde_json::Value,
        api: &'a dyn ToolApi,
        ctx: &'a Context,
    ) -> LocalBoxFuture<'a, Result<ToolResult>> {
        Box::pin(async move {
            let pending = Arc::new(Mutex::new(PendingProgress::default()));
            let wake = Arc::new(tokio::sync::Notify::new());
            let on_update: Arc<
                dyn Fn(AgentToolResult, Option<AgentHarnessToolUpdateOptions>) + Send + Sync,
            > = {
                let pending = pending.clone();
                let wake = wake.clone();
                Arc::new(move |partial, options| {
                    let mut state = pending.lock().unwrap_or_else(|error| error.into_inner());
                    state.latest = Some(partial);
                    state.checkpoint |=
                        options.and_then(|options| options.checkpoint) == Some(true);
                    drop(state);
                    wake.notify_one();
                })
            };
            let invocation: Arc<dyn AgentHarnessToolInvocation> = Arc::new(LocalInvocation {
                invocation_id: api.call_id().to_owned(),
                operation_id: api.task_id().to_string(),
                turn_id: api.conversation_id().to_string(),
                memo: Mutex::new(HashMap::new()),
            });
            let prepared = match &self.tool.prepare_arguments {
                Some(prepare) => prepare(args),
                None => Ok(args),
            };
            let turn = ExecutionToolContext {
                env: self.env.clone(),
                post_mutate: None,
            };
            let run = async {
                (self.tool.execute)(
                    api.call_id().to_owned(),
                    prepared?,
                    on_update,
                    turn,
                    invocation,
                    ctx.clone(),
                )
                .await
            };
            tokio::pin!(run);
            let mut last_progress: Option<Instant> = None;
            let flush =
                |pending: &Arc<Mutex<PendingProgress>>, force: bool, last: &mut Option<Instant>| {
                    let mut state = pending.lock().unwrap_or_else(|error| error.into_inner());
                    let due = force
                        || state.checkpoint
                        || last.is_none_or(|at| at.elapsed().as_millis() >= 100);
                    if !due {
                        return None;
                    }
                    state.checkpoint = false;
                    let partial = state.latest.take()?;
                    *last = Some(Instant::now());
                    Some((text_of(&partial.content), json_value(&partial.details)))
                };
            let outcome = loop {
                tokio::select! {
                    outcome = &mut run => break outcome,
                    () = wake.notified() => {
                        if let Some((progress, details)) = flush(&pending, false, &mut last_progress) {
                            api.progress(Box::new(move |slot: &mut ToolProgress| {
                                if !progress.is_empty() { slot.progress = Some(progress); }
                                if details.is_some() { slot.details = details; }
                            }), ctx).await?;
                        }
                    }
                }
            };
            if let Some((progress, details)) = flush(&pending, true, &mut last_progress) {
                api.progress(
                    Box::new(move |slot: &mut ToolProgress| {
                        if !progress.is_empty() {
                            slot.progress = Some(progress);
                        }
                        if details.is_some() {
                            slot.details = details;
                        }
                    }),
                    ctx,
                )
                .await?;
            }
            match outcome {
                Ok(result) => Ok(ToolResult {
                    details: json_value(&result.details),
                    control: (result.terminate == Some(true)).then(|| ToolControl {
                        terminate: Some(True),
                        ..ToolControl::default()
                    }),
                    content: Some(tool_content(result.content)),
                    ..ToolResult::default()
                }),
                Err(error) if ctx.is_aborted() => Err(Error::Forbidden(error)),
                Err(error) => Ok(ToolResult {
                    content: Some(vec![ToolContent::Text { text: error }]),
                    is_error: Some(true),
                    ..ToolResult::default()
                }),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declarations_mirror_the_mini_tools_with_replay_and_retention() {
        let env: Arc<dyn ExecutionEnv> = Arc::new(maho_agent::NodeExecutionEnv::new(
            std::env::temp_dir().to_string_lossy(),
        ));
        let tools = create_micro_tools(env).expect("micro tools");
        let shape: Vec<(String, bool, Option<Retain>)> = tools
            .declarations
            .iter()
            .map(|tool| {
                (
                    tool.name().to_owned(),
                    matches!(tool.replay(), Replay::Safe),
                    tool.output().retain,
                )
            })
            .collect();
        assert_eq!(
            shape,
            [
                ("read".to_owned(), true, Some(Retain::Head)),
                ("bash".to_owned(), false, Some(Retain::Tail)),
                ("edit".to_owned(), false, Some(Retain::Head)),
                ("write".to_owned(), false, Some(Retain::Head)),
            ]
        );
        for tool in &tools.declarations {
            assert_eq!(tool.output().max_bytes, Some(128 * 1024));
            assert_eq!(tool.output().max_lines, Some(2500));
            assert!(tool.parameters().get("properties").is_some());
        }
        let mut names: Vec<&String> = tools.model_metadata.keys().collect();
        names.sort();
        assert_eq!(names, ["bash", "edit", "read", "write"]);
    }

    #[test]
    fn only_text_and_image_blocks_reach_the_tool_result() {
        let content = vec![
            ContentBlock::text("a"),
            ContentBlock::Image(maho_ai::types::ImageContent {
                data: "AA==".to_owned(),
                mime_type: "image/png".to_owned(),
            }),
            ContentBlock::text("b"),
        ];
        assert_eq!(text_of(&content), "ab");
        assert_eq!(tool_content(content).len(), 3);
        assert_eq!(json_value(&serde_json::Value::Null), None);
        assert!(json_value(&serde_json::json!({"k": 1})).is_some());
    }
}
