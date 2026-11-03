//! Port of senpi `experimental/micro/lock-gated-storage.ts` (target 59d58cee).
//!
//! The micro session is written only through its storage's `commit` and `truncate`. Gating both on
//! the session lock means that once another process reclaims the lock, nothing in this process (a
//! run still streaming, an abort, a later close) can append to the session it now owns. senpi wraps
//! the storage in a `Proxy`; here the wrapper implements `Storage` and forwards every read.
//!
//! A refused write fails with `Error::Storage("ELOCKLOST: ...")`: Pico3's error type has no slot for
//! a foreign error, so the `FileLockLostError` code leads the message.

use std::collections::BTreeMap;
use std::rc::Rc;

use maho_agent::harness::pico3::types::{
    Context, Conversation, DocRef, Entry, EntryScan, Error, Id, Input, JsonObject, LocalBoxFuture,
    Result, Seq, Storage, Task, TaskScan, Write,
};
use maho_core::file_lock::FileLockLostError;

/// The lock check run before every write.
pub type AssertHeld = Rc<dyn Fn() -> std::result::Result<(), FileLockLostError>>;

/// senpi `lockGatedStorage(storage, assertHeld)`.
pub fn lock_gated_storage(storage: Rc<dyn Storage>, assert_held: AssertHeld) -> LockGatedStorage {
    LockGatedStorage {
        storage,
        assert_held,
    }
}

pub struct LockGatedStorage {
    storage: Rc<dyn Storage>,
    assert_held: AssertHeld,
}

impl LockGatedStorage {
    fn check(&self) -> Result<()> {
        (self.assert_held)()
            .map_err(|error| Error::Storage(format!("{}: {error}", FileLockLostError::CODE)))
    }
}

impl Storage for LockGatedStorage {
    fn commit<'a>(
        &'a self,
        writes: &'a [Write],
        ctx: &'a Context,
    ) -> LocalBoxFuture<'a, Result<Seq>> {
        Box::pin(async move {
            self.check()?;
            self.storage.commit(writes, ctx).await
        })
    }
    fn truncate<'a>(&'a self, r#ref: DocRef, ctx: &'a Context) -> LocalBoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.check()?;
            self.storage.truncate(r#ref, ctx).await
        })
    }
    fn mint_id(&self) -> Result<Id> {
        self.storage.mint_id()
    }
    fn conversation<'a>(
        &'a self,
        id: Id,
        ctx: &'a Context,
    ) -> LocalBoxFuture<'a, Result<Option<Conversation>>> {
        self.storage.conversation(id, ctx)
    }
    fn conversations<'a>(
        &'a self,
        ctx: &'a Context,
    ) -> LocalBoxFuture<'a, Result<Vec<Conversation>>> {
        self.storage.conversations(ctx)
    }
    fn entries<'a>(
        &'a self,
        ids: &'a [Id],
        ctx: &'a Context,
    ) -> LocalBoxFuture<'a, Result<BTreeMap<Id, Entry>>> {
        self.storage.entries(ids, ctx)
    }
    fn scan_entries<'a>(
        &'a self,
        scan: EntryScan,
        ctx: &'a Context,
    ) -> LocalBoxFuture<'a, Result<Vec<Entry>>> {
        self.storage.scan_entries(scan, ctx)
    }
    fn task<'a>(&'a self, id: Id, ctx: &'a Context) -> LocalBoxFuture<'a, Result<Option<Task>>> {
        self.storage.task(id, ctx)
    }
    fn scan_tasks<'a>(
        &'a self,
        scan: TaskScan,
        ctx: &'a Context,
    ) -> LocalBoxFuture<'a, Result<Vec<Task>>> {
        self.storage.scan_tasks(scan, ctx)
    }
    fn input<'a>(&'a self, id: Id, ctx: &'a Context) -> LocalBoxFuture<'a, Result<Option<Input>>> {
        self.storage.input(id, ctx)
    }
    fn input_by_request<'a>(
        &'a self,
        conversation_id: Id,
        request_id: &'a str,
        ctx: &'a Context,
    ) -> LocalBoxFuture<'a, Result<Option<Input>>> {
        self.storage
            .input_by_request(conversation_id, request_id, ctx)
    }
    fn doc<'a>(
        &'a self,
        r#ref: DocRef,
        ctx: &'a Context,
    ) -> LocalBoxFuture<'a, Result<Option<JsonObject>>> {
        self.storage.doc(r#ref, ctx)
    }
    fn doc_as_of<'a>(
        &'a self,
        conversation_id: Id,
        at: Id,
        ctx: &'a Context,
    ) -> LocalBoxFuture<'a, Result<Option<JsonObject>>> {
        self.storage.doc_as_of(conversation_id, at, ctx)
    }
    fn close<'a>(&'a self, ctx: &'a Context) -> LocalBoxFuture<'a, Result<()>> {
        self.storage.close(ctx)
    }
}
