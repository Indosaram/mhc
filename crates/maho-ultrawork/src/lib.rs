//! Ultrawork orchestration engine.
//!
//! Classifies prompts for the `ulw` / `ultrawork` keywords (after masking inactive regions),
//! injects the core contract once per session, answers re-mentions with a short notice, re-arms on
//! demand after compaction or a contract revision change, and adds the todo sizing reminder.

mod classify;
mod engine;
mod extension;
mod keywords;
mod mask;
mod mask_phrases;
mod state;
mod texts;

pub use classify::{
    Classification, ClassifyInput, KeywordSpan, Route, Stage, Suppression, classify,
    classify_with_session_role, has_visible_keyword, visible_keyword_spans,
};
pub use engine::{Engine, InputOutcome, InputRequest, TodoResultView};
pub use extension::{DISABLE_FLAG, UltraworkExtension};
pub use state::{SessionArming, Sessions};
pub use texts::{CORE_CONTRACT, DIRECTIVE_MESSAGE_TYPE, REMINDER_MESSAGE_TYPE, mass_block};

use std::sync::{Arc, LazyLock};

static GLOBAL_SESSIONS: LazyLock<Arc<Sessions>> = LazyLock::new(Arc::default);

/// The process-wide session table the default extension and the free functions below share.
pub fn global_sessions() -> Arc<Sessions> {
    Arc::clone(&GLOBAL_SESSIONS)
}

/// Current arming state of `session` in the process-wide table.
pub fn snapshot(session: &str) -> SessionArming {
    GLOBAL_SESSIONS.snapshot(session)
}

/// Marks `session` armed at the current contract revision, clearing a pending compaction re-arm.
pub fn mark_armed(session: &str) {
    GLOBAL_SESSIONS.mark_armed(session);
}

/// Records a compaction: the session is disarmed and a re-arm waits for the next mention.
pub fn mark_compact_rearm(session: &str) {
    GLOBAL_SESSIONS.mark_compact_rearm(session);
}

/// Identifier of the shipped contract text (FNV-1a over its bytes).
pub fn contract_revision() -> u64 {
    const fn fnv(bytes: &[u8]) -> u64 {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        let mut i = 0;
        while i < bytes.len() {
            hash ^= bytes[i] as u64;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            i += 1;
        }
        hash
    }
    const REVISION: u64 = fnv(CORE_CONTRACT.as_bytes());
    REVISION
}
