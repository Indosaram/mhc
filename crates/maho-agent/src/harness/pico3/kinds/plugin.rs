use serde::{Deserialize, Serialize};
use super::super::types::*;
use super::task_api::task_api;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PluginInput { pub handler: String, pub input: JsonValue }
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum PluginPhase { #[serde(rename = "started")] Started }
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PluginCheckpoint { pub phase: PluginPhase }
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginFailureReason { MissingHandler, Threw }
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PluginFailure { pub reason: PluginFailureReason, pub detail: String }

pub struct PluginKind { token: KindToken, config: KindConfig, inflight: [String; 1] }
pub fn plugin() -> PluginKind {
    PluginKind { token: KindToken::core("pi.plugin"), config: KindConfig::default(), inflight: ["started".into()] }
}

impl Kind for PluginKind {
    type Input = PluginInput;
    type Checkpoint = PluginCheckpoint;
    type Result = JsonValue;
    type Failure = PluginFailure;
    type Aborted = JsonValue;
    type Hooks = ();
    type Slot = JsonObject;
    type Tx = dyn TaskTx;
    fn token(&self) -> &KindToken { &self.token }
    fn turn(&self) -> bool { false }
    fn config(&self) -> &KindConfig { &self.config }
    fn inflight(&self) -> &[String] { &self.inflight }
    fn slot(&self, _input: &PluginInput) -> Result<JsonObject> { Ok(JsonObject::new()) }
    fn describe(&self, _task: &Task<PluginInput, PluginCheckpoint>, _slot: Option<&JsonObject>) -> Result<JsonValue> { Ok(JsonValue::Null) }
    fn initial<'a>(&'a self, task: &'a Task<PluginInput, PluginCheckpoint>, rt: &'a dyn Runtime<(), dyn TaskTx>, ctx: &'a Context) -> LocalBoxFuture<'a, Result<Step<PluginCheckpoint, JsonValue, PluginFailure>>> {
        Box::pin(async move {
            rt.commit(Box::new(|tx, _, _| Box::pin(async move {
                tx.checkpoint(Checkpoint { phase: "started".into(), fields: JsonObject::new() })?;
                Ok(JsonValue::Null)
            })), ctx).await?;
            run(task, rt, ctx).await
        })
    }
    fn phase<'a>(&'a self, phase: &'a str, task: &'a Task<PluginInput, PluginCheckpoint>, rt: &'a dyn Runtime<(), dyn TaskTx>, ctx: &'a Context) -> LocalBoxFuture<'a, Result<Step<PluginCheckpoint, JsonValue, PluginFailure>>> {
        Box::pin(async move {
            if phase != "started" { return Err(Error::TaskContractFault { kind: "pi.plugin".into(), what: format!("unknown phase {phase}") }); }
            run(task, rt, ctx).await
        })
    }
    fn abort<'a>(&'a self, _task: &'a Task<PluginInput, PluginCheckpoint>, _rt: &'a dyn Runtime<(), dyn TaskTx>, _ctx: &'a Context) -> LocalBoxFuture<'a, Result<AbortClosure>> {
        Box::pin(async { Ok(Box::new(|_, _, _| Box::pin(async { Ok(JsonValue::Null) })) as AbortClosure) })
    }
}

async fn run(task: &Task<PluginInput, PluginCheckpoint>, runtime: &dyn Runtime<(), dyn TaskTx>, ctx: &Context) -> Result<Step<PluginCheckpoint, JsonValue, PluginFailure>> {
    let Some(handler) = runtime.plugins().get(&task.input.handler) else {
        return Ok(done(Completion::Failed { failure: PluginFailure { reason: PluginFailureReason::MissingHandler, detail: task.input.handler.clone() } }));
    };
    let erased: Task = Stored::from_json(to_stored(task)?.as_json().clone()).decode()?;
    let api = task_api(&erased, runtime);
    let completion = handler_completion(handler.call(task.input.input.clone(), &api, ctx).await, ctx)?;
    Ok(done(completion))
}

fn handler_completion(result: Result<JsonValue>, ctx: &Context) -> Result<Completion<JsonValue, PluginFailure>> {
    match result {
        Ok(value) => Ok(Completion::Completed { result: to_stored(&value)?.as_json().clone() }),
        Err(error) if ctx.is_aborted() => Err(error),
        Err(error) => Ok(Completion::Failed { failure: PluginFailure { reason: PluginFailureReason::Threw, detail: error.to_string() } }),
    }
}

fn done(completion: Completion<JsonValue, PluginFailure>) -> Step<PluginCheckpoint, JsonValue, PluginFailure> {
    Step::Done(Box::new(move |_, _, _| Box::pin(async move { Ok(completion) })))
}

#[cfg(test)]
#[path = "plugin_tests.rs"]
mod tests;
