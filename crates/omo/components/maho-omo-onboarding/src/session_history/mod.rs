//! Onboarding session-history scan (upstream `onboarding/session-history/*`, `index.ts`, `runtime.ts`).
//!
//! The Rust port is synchronous and runs on the caller's thread; the tool and RPC shells hand it to a worker.

pub mod agent_transcripts;
pub mod codex_canonical_turns;
pub mod constants;
pub mod digest_store;
pub mod git_identity;
pub mod newest_sessions;
pub mod omo_session_discovery;
pub mod project_candidates;
pub mod project_paths;
pub mod recent_sessions;
pub mod rpc;
pub mod scan_session_history;
pub mod session_digest;
pub mod session_history_fs;
pub mod session_history_scanner;
pub mod tool;
pub mod tool_runtime;
pub mod transcript_cwd;
pub mod transcript_discovery;
pub mod transcript_parser;
pub mod transcript_reader;
pub mod transcript_records;
pub mod types;

pub use agent_transcripts::default_agent_homes;
pub use constants::{
    DEFAULT_RECENT_WINDOW_MS, MAX_DISCOVERY_OPERATIONS_PER_SOURCE, MAX_IMPORT_TRANSCRIPTS,
    MAX_METADATA_BYTES_PER_SOURCE, MAX_SESSION_DIGEST_BYTES, MAX_TRANSCRIPTS_PER_SOURCE,
};
pub use digest_store::{
    FileSessionDigestStore, MemorySessionDigestStore, SessionDigestStore,
    create_memory_session_digest_store, create_session_digest_store,
};
pub use recent_sessions::RecentSessionsRun;
pub use scan_session_history::{
    ScanEventKind, ScanItem, ScanSessionHistoryOptions, SessionHistoryScanEvent,
    SessionHistoryScanSource, SessionScanItem, collect_scan_session_history, scan_session_history,
};
pub use session_history_fs::{
    FileKind, FileStats, NodeSessionHistoryFs, SessionHistoryFile, SessionHistoryFs,
    node_session_history_fs,
};
pub use session_history_scanner::{SessionHistoryScanner, create_session_history_scanner};
pub use transcript_parser::parse_agent_session_transcript;
pub use types::*;
