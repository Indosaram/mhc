//! `ReleasableSession` over a real `AgentSession` (senpi `session-release.ts` reads `AgentSession` directly; the
//! Rust release works against the trait in `session_release_interrupt`, and this is the adapter the host lends it).
//!
//! Gaps against upstream, named where they sit: `AgentSession` has no `externalAdmission` ledger, so admission
//! open/closed is only recorded here (`admission_closed`) and no deliveries are ever pending; it has no
//! `whenQueueClearRecorded` (its `clear_queue` is synchronous), and `SessionManager` has no `persistHeaderNow`, so the
//! header write is done here from the manager's own header and entries.
use crate::session_release_interrupt::{ReleasableSession, ReleaseWriteError, SubscriptionGuard};
use async_trait::async_trait;
use maho_core::agent_session::{AgentSession, ClearedQueue};
use maho_core::session_activity::SessionActivitySnapshot;
use maho_core::session_manager::serialize_entry;
use maho_core::session_move::ownership::{
    SessionMovedAwayError, SessionOwnership, session_ownership,
};
use serde_json::Value;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// One live session as a release sees it.
pub struct AgentSessionRelease {
    session: AgentSession,
    admission_closed: AtomicBool,
}

impl AgentSessionRelease {
    pub fn new(session: AgentSession) -> Self {
        Self {
            session,
            admission_closed: AtomicBool::new(false),
        }
    }

    /// Whether a release closed this session's admission and did not reopen it.
    pub fn admission_closed(&self) -> bool {
        self.admission_closed.load(Ordering::SeqCst)
    }

    /// The header followed by every entry, as the file holds them.
    fn file_entries(&self) -> Vec<Value> {
        self.session.with_session_manager(|manager| {
            manager
                .header()
                .into_iter()
                .chain(manager.entries())
                .collect()
        })
    }
}

#[async_trait]
impl ReleasableSession for AgentSessionRelease {
    fn activity_snapshot(&self) -> SessionActivitySnapshot {
        self.session.activity_snapshot()
    }

    fn pending_message_count(&self) -> usize {
        self.session.pending_message_count()
    }

    fn admission_pending(&self) -> Vec<String> {
        Vec::new()
    }

    fn admission_close(&self, _reason: &str) {
        self.admission_closed.store(true, Ordering::SeqCst);
    }

    fn admission_reopen(&self) {
        self.admission_closed.store(false, Ordering::SeqCst);
    }

    fn clear_queue(&self, abort_will_follow: bool) -> ClearedQueue {
        self.session.clear_queue(abort_will_follow)
    }

    fn abort_bash(&self) {
        self.session.abort_bash();
    }

    fn on_agent_start(&self, on_start: Arc<dyn Fn() + Send + Sync>) -> SubscriptionGuard {
        Box::new(self.session.subscribe(Arc::new(move |event| {
            if matches!(
                event,
                maho_ext_api::AgentSessionEvent::Agent(maho_ext_api::AgentEvent::AgentStart)
            ) {
                on_start();
            }
        })))
    }

    async fn abort(&self) {
        self.session.abort().await;
    }

    async fn wait_for_idle(&self) {
        self.session.wait_for_idle().await;
    }

    async fn when_queue_clear_recorded(&self) {}

    fn session_id(&self) -> String {
        self.session.session_id()
    }

    fn is_transcript_flushed(&self) -> bool {
        self.session
            .with_session_manager(|manager| manager.is_flushed())
    }

    async fn persist_header_now(&self) -> Result<(), String> {
        let (persisted, flushed, file) = self.session.with_session_manager(|manager| {
            (
                manager.is_persisted(),
                manager.is_flushed(),
                manager.session_file().map(str::to_owned),
            )
        });
        let Some(file) = file.filter(|_| persisted && !flushed) else {
            return Ok(());
        };
        let entries = self.file_entries();
        if entries.is_empty() {
            return Err("session has no header to write".to_owned());
        }
        let mut text = String::new();
        for entry in &entries {
            text.push_str(&serialize_entry(entry));
            text.push('\n');
        }
        if let Some(parent) = std::path::Path::new(&file).parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|error| error.to_string())?;
        }
        tokio::fs::write(&file, text)
            .await
            .map_err(|error| error.to_string())
    }

    fn append_custom_entry(&self, custom_type: &str, data: Value) -> Result<(), ReleaseWriteError> {
        let (file, persisted, flushed) = self.session.with_session_manager(|manager| {
            (
                manager.session_file().map(str::to_owned),
                manager.is_persisted(),
                manager.is_flushed(),
            )
        });
        if let SessionOwnership::Refused(refused) = session_ownership(&self.file_entries()) {
            return Err(ReleaseWriteError::MovedAway(SessionMovedAwayError::new(
                file.as_deref().unwrap_or_default(),
                refused,
            )));
        }
        let entry = self
            .session
            .with_session_manager_mut(|manager| manager.append_custom(custom_type, Some(data)));
        // A manager that is not flushed buffers the entry until the first assistant message; the header write above
        // put the file on disk, so the entry is appended to it here: on disk or not at all.
        if persisted
            && !flushed
            && let Some(file) = file
        {
            use std::io::Write;
            let mut line = serialize_entry(&entry);
            line.push('\n');
            std::fs::OpenOptions::new()
                .append(true)
                .open(&file)
                .and_then(|mut handle| handle.write_all(line.as_bytes()))
                .map_err(|error| ReleaseWriteError::Other(error.to_string()))?;
        }
        Ok(())
    }
}
