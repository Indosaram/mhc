//! Port of `pico3/kinds/task-api.ts`: task capabilities, without tool output.
use serde::de::DeserializeOwned;
use super::super::types::*;

pub struct TaskApi<'r, H, Tx: TaskTx + ?Sized> {
    task_id: Id,
    conversation_id: Id,
    runtime: &'r dyn Runtime<H, Tx>,
}

pub fn task_api<'r, H, Tx: TaskTx + ?Sized>(task: &Task, runtime: &'r dyn Runtime<H, Tx>) -> TaskApi<'r, H, Tx> {
    TaskApi { task_id: task.id, conversation_id: task.conversation_id, runtime }
}

fn decode<T: DeserializeOwned>(value: JsonValue) -> Result<T> {
    Stored::<T>::from_json(value).decode()
}

fn check_kind(task: &Task, kind: &KindToken) -> Result<()> {
    if task.kind != kind.name() {
        return Err(Error::Forbidden(format!("task {} is not kind {}", task.id, kind.name())));
    }
    Ok(())
}

fn registered_kind<H, Tx: TaskTx + ?Sized>(runtime: &dyn Runtime<H, Tx>, kind: &KindToken) -> Result<()> {
    if !runtime.kinds().get(kind.name()).is_some_and(|registered| registered.same_identity(kind)) {
        return Err(Error::Forbidden("task(): pass a registered kind token".into()));
    }
    Ok(())
}

impl<H, Tx: TaskTx + ?Sized> ToolApi for TaskApi<'_, H, Tx> {
    fn task_id(&self) -> Id { self.task_id }
    fn conversation_id(&self) -> Id { self.conversation_id }
    fn call_id(&self) -> &str { "" }
    fn stream(&self, _chunk: &[u8]) -> Result<()> {
        Err(Error::Forbidden("stream is only available to tools".into()))
    }
    fn progress<'a>(&'a self, _update: Box<dyn FnOnce(&mut ToolProgress) + 'a>, _ctx: &'a Context) -> LocalBoxFuture<'a, Result<()>> {
        Box::pin(async { Err(Error::Forbidden("progress is only available to tools".into())) })
    }
    fn memo<'a>(&'a self, _name: &'a str, _candidate: Option<JsonValue>, _ctx: &'a Context) -> LocalBoxFuture<'a, Result<Option<JsonValue>>> {
        Box::pin(async { Err(Error::Forbidden("memo is only available to tools".into())) })
    }
    fn conversation<'a>(&'a self, spec: OwnedConversationSpec, ctx: &'a Context) -> LocalBoxFuture<'a, Result<Box<dyn OwnedConversation + 'a>>> {
        Box::pin(async move {
            let id = self.runtime.create_owned_conversation(spec, ctx).await?;
            Ok(Box::new(ConversationHandle { id, runtime: self.runtime }) as Box<dyn OwnedConversation + 'a>)
        })
    }
    fn task<'a>(&'a self, kind: &'a KindToken, input: JsonValue, mut opts: TaskOptions, ctx: &'a Context) -> LocalBoxFuture<'a, Result<TaskRef>> {
        Box::pin(async move {
            registered_kind(self.runtime, kind)?;
            // The upstream task API does not expose the conversation override.
            opts.conversation_id = None;
            let token = kind.clone();
            let value = self.runtime.commit(Box::new(move |tx, _, _| Box::pin(async move {
                let created = tx.create_task(&token, input, opts)?;
                Ok(to_stored(&created.id)?.as_json().clone())
            })), ctx).await?;
            Ok(TaskRef { id: decode(value)?, kind: kind.clone() })
        })
    }
    fn get_task<'a>(&'a self, reference: &'a TaskRef, ctx: &'a Context) -> LocalBoxFuture<'a, Result<Option<Task>>> {
        Box::pin(async move {
            registered_kind(self.runtime, &reference.kind)?;
            let id = reference.id;
            let kind = reference.kind.clone();
            decode(self.runtime.commit(Box::new(move |tx, _, _| Box::pin(async move {
                let task = tx.task(id).await?;
                if let Some(task) = &task { check_kind(task, &kind)?; }
                Ok(to_stored(&task)?.as_json().clone())
            })), ctx).await?)
        })
    }
    fn wait_for_task<'a>(&'a self, reference: &'a TaskRef, ctx: &'a Context) -> LocalBoxFuture<'a, Result<Task>> {
        Box::pin(async move {
            registered_kind(self.runtime, &reference.kind)?;
            let task = self.runtime.wait_for_task(reference.id, ctx).await?;
            check_kind(&task, &reference.kind)?;
            Ok(task)
        })
    }
    fn slot<'a>(&'a self, reference: &'a TaskRef, ctx: &'a Context) -> LocalBoxFuture<'a, Result<Option<JsonObject>>> {
        Box::pin(async move {
            registered_kind(self.runtime, &reference.kind)?;
            let id = reference.id;
            let kind = reference.kind.clone();
            decode(self.runtime.commit(Box::new(move |tx, _, _| Box::pin(async move {
                let Some(task) = tx.task(id).await? else { return Ok(JsonValue::Null); };
                check_kind(&task, &kind)?;
                let snapshot = tx.snapshot(DocRef::Sticky { conversation_id: task.conversation_id })?;
                let DocumentSnapshot::Sticky(sticky) = snapshot else {
                    return Err(Error::InvalidShape("sticky snapshot returned another document kind".into()));
                };
                Ok(to_stored(&sticky.tasks.get(&id.to_string()))?.as_json().clone())
            })), ctx).await?)
        })
    }
}

struct ConversationHandle<'r, H, Tx: TaskTx + ?Sized> { id: Id, runtime: &'r dyn Runtime<H, Tx> }
impl<H, Tx: TaskTx + ?Sized> OwnedConversation for ConversationHandle<'_, H, Tx> {
    fn id(&self) -> Id { self.id }
    fn send<'a>(&'a self, input: SendInput, ctx: &'a Context) -> LocalBoxFuture<'a, Result<Box<dyn InputHandle + 'a>>> {
        Box::pin(async move {
            let id = self.runtime.send_owned(self.id, input, ctx).await?;
            Ok(Box::new(OwnedInput { id, runtime: self.runtime }) as Box<dyn InputHandle + 'a>)
        })
    }
    fn abort<'a>(&'a self, ctx: &'a Context) -> LocalBoxFuture<'a, Result<()>> {
        self.runtime.abort_conversation(self.id, ctx)
    }
}
struct OwnedInput<'r, H, Tx: TaskTx + ?Sized> { id: Id, runtime: &'r dyn Runtime<H, Tx> }
impl<H, Tx: TaskTx + ?Sized> InputHandle for OwnedInput<'_, H, Tx> {
    fn id(&self) -> Id { self.id }
    fn wait<'a>(&'a self, ctx: &'a Context) -> LocalBoxFuture<'a, Result<Input>> {
        self.runtime.wait_for_input(self.id, ctx)
    }
    fn result<'a>(&'a self, ctx: &'a Context) -> LocalBoxFuture<'a, Result<Option<Input>>> {
        Box::pin(async move {
            let id = self.id;
            decode(self.runtime.commit(Box::new(move |tx, _, _| Box::pin(async move {
                Ok(to_stored(&tx.input(id).await?)?.as_json().clone())
            })), ctx).await?)
        })
    }
}

#[cfg(test)]
#[path = "task_api_tests.rs"]
pub(crate) mod tests;
