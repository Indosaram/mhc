//! Port of `pico3/kinds/tool.ts`; effects begin only after the started commit.
use std::{cell::{Cell, RefCell}, rc::Rc};
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use super::super::{bounded::{Bounded, Retain}, membrane::Document, types::*};
use super::task_api::{task_api, TaskApi};

pub type StoredToolCall = Stored<ToolCall>;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolInput { pub assistant: Id, pub call: StoredToolCall, pub offered: Vec<String>, pub index: usize }
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolCheckpoint { pub phase: Started, pub replay: Replay, pub call: StoredToolCall }
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum Started { #[serde(rename = "started")] Started }
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolTaskResult { pub entry: Id, #[serde(skip_serializing_if = "Option::is_none")] pub control: Option<ToolControl> }
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolAborted { pub entry: Id }
#[derive(Clone, Debug, Default)]
pub struct BeforeToolResult { pub call: Option<StoredToolCall>, pub block: Option<String> }
pub type BeforeTool = Rc<dyn for<'a> Fn(StoredToolCall, &'a dyn BeforeToolApi, &'a Context) -> LocalBoxFuture<'a, Result<Option<BeforeToolResult>>>>;
pub type AfterTool = Rc<dyn for<'a> Fn(StoredToolCall, ToolResult, HookInfo, String, &'a Context) -> LocalBoxFuture<'a, Result<Option<ToolResult>>>>;
#[derive(Default)]
pub struct ToolHooks { pub before_tool: Option<BeforeTool>, pub after_tool: Option<AfterTool> }
type T = Task<ToolInput, ToolCheckpoint>;
type Rt = dyn Runtime<ToolHooks, dyn CoreTx>;
type Out = Closure<ToolTaskResult, JsonValue, dyn CoreTx>;
type S = Step<ToolCheckpoint, ToolTaskResult, JsonValue, dyn CoreTx>;

fn json<T: Serialize>(v: &T) -> Result<JsonValue> { Ok(to_stored(v)?.as_json().clone()) }
fn decode<T: serde::de::DeserializeOwned>(v: &JsonValue) -> Result<T> { Stored::from_json(v.clone()).decode() }
fn call_object(call: &StoredToolCall) -> Result<JsonObject> { decode(call.as_json()) }
fn call_string(call: &StoredToolCall, key: &str) -> Result<String> {
    let object = call_object(call)?;
    decode(object.get(key).ok_or_else(|| Error::InvalidShape(format!("tool call missing {key}")))?)
}
fn arguments(call: &StoredToolCall) -> Result<JsonValue> {
    call_object(call)?.get("arguments").cloned().ok_or_else(|| Error::InvalidShape("tool call missing arguments".into()))
}
fn same_identity(a: &StoredToolCall, b: &StoredToolCall) -> Result<bool> {
    let a = call_object(a)?; let b = call_object(b)?;
    Ok(["id", "name", "namespace"].iter().all(|key| a.get(key) == b.get(key)))
}
fn synthetic(text: String, code: &str) -> ToolResult {
    ToolResult { content: Some(vec![ToolContent::Text { text: text.clone() }]), is_error: Some(true),
        diagnostics: Some(vec![ToolDiagnostic { severity: Severity::Error, message: text, code: Some(code.into()) }]), ..ToolResult::default() }
}
/// Strict TypeBox error walk, without nullable normalization or coercion.
/// Requires the AI owner to expose its existing engine through `strict_errors`.
pub fn invalid(declaration: &dyn AnyToolDeclaration, call: &StoredToolCall) -> Result<Option<String>> {
    let schema = serde_json::from_str(&serde_json::to_string(declaration.parameters())?)?;
    let value = serde_json::from_str(&serde_json::to_string(&arguments(call)?)?)?;
    let errors = maho_ai::utils::validation::strict_errors(&schema, &value);
    Ok((!errors.is_empty()).then(|| errors.into_iter().map(|(path, message)| format!("{}: {message}", if path.is_empty() { "/" } else { &path })).collect::<Vec<_>>().join("; ")))
}
fn replay_allowed(old: Replay, new: Replay) -> bool { matches!((old, new), (Replay::Safe, Replay::Safe)) }

pub struct ToolKind { token: KindToken, config: KindConfig, inflight: [String; 1] }
pub fn tool() -> ToolKind { ToolKind { token: KindToken::core("pi.tool"), config: KindConfig::default(), inflight: ["started".into()] } }
impl Kind for ToolKind {
    type Input = ToolInput; type Checkpoint = ToolCheckpoint; type Result = ToolTaskResult; type Failure = JsonValue;
    type Aborted = ToolAborted; type Hooks = ToolHooks; type Slot = JsonObject; type Tx = dyn CoreTx;
    fn token(&self) -> &KindToken { &self.token }
    fn turn(&self) -> bool { true }
    fn config(&self) -> &KindConfig { &self.config }
    fn inflight(&self) -> &[String] { &self.inflight }
    fn slot(&self, _: &ToolInput) -> Result<JsonObject> { Ok(JsonObject::new()) }
    fn describe(&self, _: &T, _: Option<&JsonObject>) -> Result<JsonValue> { Ok(JsonValue::Null) }
    fn initial<'a>(&'a self, task: &'a T, rt: &'a Rt, ctx: &'a Context) -> LocalBoxFuture<'a, Result<S>> {
        Box::pin(async move {
            let call = &task.input.call; let name = call_string(call, "name")?;
            if !task.input.offered.contains(&name) { return Ok(Step::Done(close(task, synthetic(format!("tool {name} was not offered"), "not_offered"), OutputLimits::default(), rt.now(), call.clone(), None)?)); }
            let Some(declaration) = rt.tools().get(&name) else { return Ok(Step::Done(close(task, synthetic(format!("tool {name} is not registered"), "missing_tool"), OutputLimits::default(), rt.now(), call.clone(), None)?)); };
            if let Some(bad) = invalid(declaration.as_ref(), call)? { return Ok(Step::Done(close(task, synthetic(format!("invalid arguments: {bad}"), "invalid_arguments"), declaration.output(), rt.now(), call.clone(), None)?)); }
            let hooked = before_tool(task, rt, call.clone(), ctx).await?;
            if let Some(block) = hooked.block { return Ok(Step::Done(close(task, synthetic(format!("blocked: {block}"), "blocked"), declaration.output(), rt.now(), call.clone(), None)?)); }
            let final_call = hooked.call.unwrap_or_else(|| call.clone());
            if !same_identity(call, &final_call)? { return Ok(Step::Done(close(task, synthetic("blocked: call identity changed".into(), "blocked"), declaration.output(), rt.now(), call.clone(), None)?)); }
            if let Some(bad) = invalid(declaration.as_ref(), &final_call)? { return Ok(Step::Done(close(task, synthetic(format!("invalid arguments after hook: {bad}"), "invalid_arguments"), declaration.output(), rt.now(), call.clone(), None)?)); }
            let cp = ToolCheckpoint { phase: Started::Started, replay: declaration.replay(), call: final_call.clone() };
            let checkpoint: Checkpoint = decode(&json(&cp)?)?; let index = task.input.index;
            let call_id = call_string(&final_call, "id")?;
            rt.commit(Box::new(move |tx, current, _| Box::pin(async move {
                tx.checkpoint(checkpoint)?;
                let slot = tx.tool_slot(current.conversation_id, index)?;
                slot.set("status", &json(&ToolStatus::Running)?)?; slot.delete("waitingOn")?;
                tx.emit(ViewEvent::Core(CoreViewEvent::ToolStarted { task_id: current.id, call_id, name }))?;
                Ok(JsonValue::Null)
            })), ctx).await?;
            Ok(Step::Done(invoke(task, final_call, declaration.as_ref(), rt, ctx).await?))
        })
    }
    fn phase<'a>(&'a self, phase: &'a str, task: &'a T, rt: &'a Rt, ctx: &'a Context) -> LocalBoxFuture<'a, Result<S>> {
        Box::pin(async move {
            if phase != "started" { return Err(Error::TaskContractFault { kind: "pi.tool".into(), what: format!("unknown phase {phase}") }); }
            let cp = task.checkpoint.as_ref().ok_or_else(|| Error::InvalidShape("started tool lacks checkpoint".into()))?;
            let name = call_string(&cp.call, "name")?;
            let Some(declaration) = rt.tools().get(&name) else { return Ok(Step::Done(close(task, synthetic(format!("tool {name} unavailable after restart"), "unavailable"), OutputLimits::default(), rt.now(), cp.call.clone(), None)?)); };
            if !replay_allowed(cp.replay, declaration.replay()) || invalid(declaration.as_ref(), &cp.call)?.is_some() {
                return Ok(Step::Done(close(task, synthetic(format!("tool {name} was interrupted"), "interrupted"), declaration.output(), rt.now(), cp.call.clone(), None)?));
            }
            Ok(Step::Done(invoke(task, cp.call.clone(), declaration.as_ref(), rt, ctx).await?))
        })
    }
    fn abort<'a>(&'a self, task: &'a T, rt: &'a Rt, ctx: &'a Context) -> LocalBoxFuture<'a, Result<AbortClosure<ToolAborted, dyn CoreTx>>> {
        Box::pin(async move {
            for &conversation_id in &task.owns {
                let children = rt.commit(Box::new(move |tx, _, _| Box::pin(async move {
                    json(&tx.tasks(TaskScan { conversation_id: Some(conversation_id), status: Some(vec![TaskStatus::Pending, TaskStatus::Running]), kind: None }).await?)
                })), ctx).await?;
                for child in decode::<Vec<Task>>(&children)? { if child.background.is_none() { rt.abort_task(child.id, ctx).await?; } }
            }
            let call = task.checkpoint.as_ref().map_or_else(|| task.input.call.clone(), |cp| cp.call.clone());
            let index = task.input.index; let now = rt.now();
            Ok(Box::new(move |tx: &mut dyn CoreTx, current: &Task, _: &Context| Box::pin(async move {
                let mut entry = result_entry(&call, &synthetic("tool aborted".into(), "aborted"), now)?;
                entry.data = Some(decode(&json(&serde_json::json!({"diagnostics":[{"severity":"error","message":"aborted","code":"aborted"}]}))?)?);
                let entry = tx.append_entry(current.conversation_id, entry)?;
                let tools = tx.sticky(current.conversation_id)?.get("turn")?.ok_or_else(|| Error::InvalidShape("missing turn".into()))?.get("tools")?.ok_or_else(|| Error::InvalidShape("missing tools".into()))?;
                if let Some(slot) = tools.get(index)? { slot.set("status", &json(&ToolStatus::Aborted)?)?; slot.set("entry", &json(&entry)?)?; slot.delete("waitingOn")?; }
                tx.emit(ViewEvent::Core(CoreViewEvent::ToolAborted { task_id: current.id, call_id: call_string(&call, "id")?, entry }))?;
                Ok(ToolAborted { entry })
            })) as AbortClosure<ToolAborted, dyn CoreTx>)
        })
    }
}

struct BeforeApi<'a> { rt: &'a Rt, namespace: Namespace, info: HookInfo, call_id: String, index: usize }
impl BeforeToolApi for BeforeApi<'_> {
    fn info(&self) -> &HookInfo { &self.info }
    fn call_id(&self) -> &str { &self.call_id }
    fn waiting<'a>(&'a self, ctx: &'a Context) -> LocalBoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let namespace = self.namespace.clone(); let index = self.index; let call_id = self.call_id.clone();
            self.rt.commit(Box::new(move |tx, current, _| Box::pin(async move {
                tx.plugins(&namespace)?;
                let slot = tx.tool_slot(current.conversation_id, index)?;
                if slot.get("waitingOn")?.map(|d| d.value()).transpose()? != Some(json(&namespace.id())?) {
                    slot.set("waitingOn", &json(&namespace.id())?)?;
                    tx.emit(ViewEvent::Core(CoreViewEvent::ToolWaiting { task_id: current.id, call_id, on: namespace.id().into() }))?;
                }
                Ok(JsonValue::Null)
            })), ctx).await?; Ok(())
        })
    }
    fn memo<'a>(&'a self, name: &'a str, candidate: Option<JsonValue>, ctx: &'a Context) -> LocalBoxFuture<'a, Result<Option<JsonValue>>> {
        memo(self.rt, self.index, format!("hook:{}:{name}", self.namespace.id()), candidate, Some(self.namespace.clone()), ctx)
    }
    fn emit<'a>(&'a self, name: &'a str, data: JsonValue, ctx: &'a Context) -> LocalBoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let namespace = self.namespace.clone(); let name = name.to_owned();
            self.rt.commit(Box::new(move |tx, _, _| Box::pin(async move { tx.plugins(&namespace)?; tx.emit(ViewEvent::plugin(&namespace, &name, data)?)?; Ok(JsonValue::Null) })), ctx).await?; Ok(())
        })
    }
}
async fn before_tool(task: &T, rt: &Rt, call: StoredToolCall, ctx: &Context) -> Result<BeforeToolResult> {
    let mut current = call.clone();
    for binding in rt.hooks().handlers() {
        let Some(handler) = &binding.handlers.before_tool else { continue; };
        let api = BeforeApi { rt, namespace: binding.namespace, info: binding.api, call_id: call_string(&call, "id")?, index: task.input.index };
        match handler(current.clone(), &api, ctx).await {
            Ok(Some(value)) => { if value.block.is_some() { return Ok(value); } if let Some(call) = value.call { current = call; } }
            Ok(None) => {}
            Err(error) if ctx.is_aborted() => return Err(error),
            Err(error) => return Ok(BeforeToolResult { block: Some(format!("hook threw: {error}")), call: None }),
        }
    }
    Ok(BeforeToolResult { call: Some(current), block: None })
}
fn memo<'a>(rt: &'a Rt, index: usize, key: String, candidate: Option<JsonValue>, namespace: Option<Namespace>, ctx: &'a Context) -> LocalBoxFuture<'a, Result<Option<JsonValue>>> {
    Box::pin(async move {
        // Encode presence separately: JSON null is a memo winner, not absence.
        let value = rt.commit(Box::new(move |tx, current, _| Box::pin(async move {
            if let Some(namespace) = namespace { tx.plugins(&namespace)?; }
            let slot = tx.tool_slot(current.conversation_id, index)?;
            let mut object: JsonObject = decode(&slot.value()?)?;
            let existing = match object.get("memos") { None => None, Some(JsonValue::Object(m)) => m.iter().find(|(k, _)| k.as_str() == Some(key.as_str())).map(|(_, v)| v.clone()), Some(_) => return Err(Error::InvalidShape("memos must be an object".into())) };
            let value = match (existing, candidate) { (Some(v), _) => Some(v), (None, Some(v)) => { let v = memo_once(&mut object, &key, v)?; slot.set("memos", object.get("memos").ok_or_else(|| Error::InvalidShape("memo insertion missing".into()))?)?; Some(v) }, (None, None) => None };
            json(&value.into_iter().collect::<Vec<_>>())
        })), ctx).await?;
        Ok(decode::<Vec<JsonValue>>(&value)?.into_iter().next())
    })
}

struct StreamState { buffer: RefCell<Bounded>, streamed: Cell<bool>, revision: Cell<u64>, done: Cell<bool>, notify: Notify }
struct InvokeApi<'a> { base: TaskApi<'a, ToolHooks, dyn CoreTx>, rt: &'a Rt, state: &'a StreamState, call_id: String, index: usize }
impl ToolApi for InvokeApi<'_> {
    fn task_id(&self) -> Id { self.base.task_id() }
    fn conversation_id(&self) -> Id { self.base.conversation_id() }
    fn call_id(&self) -> &str { &self.call_id }
    fn stream(&self, chunk: &[u8]) -> Result<()> { self.state.streamed.set(true); self.state.buffer.borrow_mut().push(chunk); self.state.revision.set(self.state.revision.get().saturating_add(1)); self.state.notify.notify_one(); Ok(()) }
    fn progress<'a>(&'a self, update: Box<dyn FnOnce(&mut ToolProgress) + 'a>, ctx: &'a Context) -> LocalBoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let index = self.index;
            self.rt.commit(Box::new(move |tx, current, _| Box::pin(async move {
                let slot = tx.tool_slot(current.conversation_id, index)?;
                let s: ToolSlot = decode(&slot.value()?)?;
                let mut free = ToolProgress { progress: s.progress, details: s.details, continued_by: s.continued_by }; update(&mut free);
                set_optional(&slot, "progress", free.progress.as_ref())?; set_optional(&slot, "details", free.details.as_ref())?; set_optional(&slot, "continuedBy", free.continued_by.as_ref())?;
                Ok(JsonValue::Null)
            })), ctx).await?; Ok(())
        })
    }
    fn memo<'a>(&'a self, name: &'a str, candidate: Option<JsonValue>, ctx: &'a Context) -> LocalBoxFuture<'a, Result<Option<JsonValue>>> { memo(self.rt, self.index, format!("tool:{name}"), candidate, None, ctx) }
    fn conversation<'a>(&'a self, spec: OwnedConversationSpec, ctx: &'a Context) -> LocalBoxFuture<'a, Result<Box<dyn OwnedConversation + 'a>>> { self.base.conversation(spec, ctx) }
    fn task<'a>(&'a self, kind: &'a KindToken, input: JsonValue, opts: TaskOptions, ctx: &'a Context) -> LocalBoxFuture<'a, Result<TaskRef>> { self.base.task(kind, input, opts, ctx) }
    fn get_task<'a>(&'a self, reference: &'a TaskRef, ctx: &'a Context) -> LocalBoxFuture<'a, Result<Option<Task>>> { self.base.get_task(reference, ctx) }
    fn wait_for_task<'a>(&'a self, reference: &'a TaskRef, ctx: &'a Context) -> LocalBoxFuture<'a, Result<Task>> { self.base.wait_for_task(reference, ctx) }
    fn slot<'a>(&'a self, reference: &'a TaskRef, ctx: &'a Context) -> LocalBoxFuture<'a, Result<Option<JsonObject>>> { self.base.slot(reference, ctx) }
}
fn set_optional<T: Serialize>(slot: &Document, key: &str, value: Option<&T>) -> Result<()> { match value { Some(v) => slot.set(key, &json(v)?), None => slot.delete(key) } }
fn bounds(output: &OutputLimits) -> (usize, usize, Retain) { (output.max_bytes.unwrap_or(64 * 1024), output.max_lines.unwrap_or(200), output.retain.unwrap_or(Retain::Head)) }
async fn invoke(task: &T, call: StoredToolCall, declaration: &dyn AnyToolDeclaration, rt: &Rt, ctx: &Context) -> Result<Out> {
    let output = declaration.output(); let (bytes, lines, retain) = bounds(&output);
    let state = StreamState { buffer: RefCell::new(Bounded::new(bytes, lines, retain)), streamed: Cell::new(false), revision: Cell::new(0), done: Cell::new(false), notify: Notify::new() };
    let erased: Task = decode(&json(task)?)?;
    let api = InvokeApi { base: task_api(&erased, rt), rt, state: &state, call_id: call_string(&call, "id")?, index: task.input.index };
    let args = arguments(&call)?;
    let execute = async { let result = declaration.execute(args, &api, ctx).await; state.done.set(true); state.notify.notify_one(); result };
    let flush = async {
        let mut revision = 0; let mut last_flush = 0;
        loop {
            let notified = state.notify.notified(); tokio::pin!(notified); notified.as_mut().enable();
            if revision != state.revision.get() && (state.done.get() || rt.now().saturating_sub(last_flush) >= 100) {
                revision = state.revision.get(); last_flush = rt.now();
                let text = state.buffer.borrow().text().into_owned(); let index = task.input.index;
                rt.commit(Box::new(move |tx, current, _| Box::pin(async move { tx.tool_slot(current.conversation_id, index)?.set("output", &json(&text)?)?; Ok(JsonValue::Null) })), ctx).await?;
                continue;
            }
            if state.done.get() { break; }
            notified.await;
        }
        Ok::<(), Error>(())
    };
    // A persistence error drops the effect future, rather than allowing it to run past revocation.
    let (executed, ()) = futures::future::try_join(async { Ok::<_, Error>(execute.await) }, flush).await?;
    let mut result = match executed { Ok(result) => result, Err(error) if ctx.is_aborted() => return Err(error), Err(error) => synthetic(format!("tool threw: {error}"), "threw") };
    let truncated = if state.streamed.get() {
        let buffer = state.buffer.borrow();
        if result.content.is_none() {
            result.content = Some(vec![ToolContent::Text { text: buffer.text().into_owned() }]);
            if buffer.dropped() > 0 { result.diagnostics.get_or_insert_with(Vec::new).push(ToolDiagnostic { severity: Severity::Warn, code: Some("truncated".into()), message: format!("{} bytes / {} lines dropped", buffer.dropped_bytes, buffer.dropped_lines) }); }
        }
        Some(Truncated { bytes: buffer.dropped_bytes, lines: buffer.dropped_lines })
    } else { None };
    let final_result = RefCell::new(result);
    rt.hooks().each(ctx, |hooks, info| {
        let call = call.clone(); let result = final_result.borrow().clone(); let call_id = api.call_id.clone();
        async move { match &hooks.after_tool { Some(handler) => handler(call, result, info, call_id, ctx).await, None => Ok(None) } }
    }, |value| { *final_result.borrow_mut() = value; false }).await?;
    close(task, final_result.into_inner(), output, rt.now(), call, truncated)
}
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
struct Truncated { bytes: usize, lines: usize }
fn result_entry(call: &StoredToolCall, result: &ToolResult, now: u64) -> Result<NewEntry> {
    let model = serde_json::json!({"role":"toolResult", "toolCallId":call_string(call,"id")?, "toolName":call_string(call,"name")?, "content":result.content.as_deref().unwrap_or(&[]), "isError":result.is_error.unwrap_or(false), "timestamp":now});
    Ok(NewEntry { kind: "pi.tool_result".into(), model: Some(vec![StoredMessage::from_json(json(&model)?)]), data: None, head: None, edits: None })
}
fn close(task: &T, raw: ToolResult, output: OutputLimits, now: u64, call: StoredToolCall, stream: Option<Truncated>) -> Result<Out> {
    let index = task.input.index;
    Ok(Box::new(move |tx, current, _| Box::pin(async move {
        let (result, returned) = bound(raw, &output);
        let stream = stream.unwrap_or_default(); let truncated = Truncated { bytes: returned.bytes.saturating_add(stream.bytes), lines: returned.lines.saturating_add(stream.lines) };
        let mut data = JsonObject::new();
        if let Some(details) = &result.details { data.insert("details".into(), details.clone()); }
        if let Some(diagnostics) = &result.diagnostics { data.insert("diagnostics".into(), json(diagnostics)?); }
        if let Some(control) = &result.control { data.insert("control".into(), json(control)?); }
        if truncated.bytes > 0 || truncated.lines > 0 { data.insert("truncated".into(), json(&truncated)?); }
        let mut entry = result_entry(&call, &result, now)?; entry.data = Some(data);
        let entry = tx.append_entry(current.conversation_id, entry)?;
        let slot = tx.tool_slot(current.conversation_id, index)?;
        slot.set("status", &json(&if result.is_error == Some(true) { ToolStatus::Error } else { ToolStatus::Done })?)?;
        slot.set("entry", &json(&entry)?)?; slot.delete("waitingOn")?;
        tx.emit(ViewEvent::Core(CoreViewEvent::ToolFinished { task_id: current.id, call_id: call_string(&call, "id")?, entry, is_error: result.is_error == Some(true), control: result.control.clone() }))?;
        if truncated.bytes > 0 || truncated.lines > 0 { tx.emit(ViewEvent::Core(CoreViewEvent::Warning { source: "tool".into(), message: format!("tool output truncated: {} bytes / {} lines dropped", truncated.bytes, truncated.lines) }))?; }
        Ok(Completion::Completed { result: ToolTaskResult { entry, control: result.control } })
    })))
}
fn bound(mut result: ToolResult, declared: &OutputLimits) -> (ToolResult, Truncated) {
    let (max_bytes, max_lines, retain) = bounds(declared);
    let blocks = result.content.take().unwrap_or_default();
    let text = blocks.iter().filter_map(|b| match b { ToolContent::Text { text } => Some(text.as_str()), ToolContent::Image { .. } => None }).collect::<String>();
    let lines: Vec<_> = text.split('\n').collect();
    let dropped_lines = lines.len().saturating_sub(max_lines);
    let text = if dropped_lines == 0 { text } else { match retain { Retain::Head => lines[..max_lines].join("\n"), Retain::Tail => lines[lines.len().saturating_sub(max_lines)..].join("\n") } };
    let bytes = text.as_bytes(); let dropped_bytes = bytes.len().saturating_sub(max_bytes);
    if dropped_bytes == 0 && dropped_lines == 0 { result.content = Some(blocks); return (result, Truncated::default()); }
    let bytes = if dropped_bytes == 0 { bytes } else { match retain { Retain::Head => &bytes[..max_bytes], Retain::Tail => &bytes[bytes.len().saturating_sub(max_bytes)..] } };
    let text = String::from_utf8_lossy(bytes.strip_prefix(&[0xef,0xbb,0xbf]).unwrap_or(bytes)).into_owned();
    let text_index = match retain { Retain::Head => blocks.iter().position(|b| matches!(b, ToolContent::Text { .. })), Retain::Tail => blocks.iter().rposition(|b| matches!(b, ToolContent::Text { .. })) };
    result.content = Some(blocks.into_iter().enumerate().filter_map(|(i,b)| match b { ToolContent::Image { .. } => Some(b), ToolContent::Text { .. } => (Some(i) == text_index).then(|| ToolContent::Text { text: text.clone() }) }).collect());
    result.diagnostics.get_or_insert_with(Vec::new).push(ToolDiagnostic { severity: Severity::Warn, code: Some("truncated".into()), message: format!("output truncated: {dropped_lines} lines, {dropped_bytes} bytes dropped") });
    (result, Truncated { bytes: dropped_bytes, lines: dropped_lines })
}

#[cfg(test)]
#[path = "tool_tests.rs"]
mod tests;
