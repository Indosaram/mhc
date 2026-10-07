//! Output caps for the sidecar's read-only tools (latest `kibitzer/tools/caps.ts`).
//!
//! Every tool result is bounded by one of these BEFORE it reaches the child. The `grep_scan*`
//! members bound the other direction: the work a scan may do before it reports `truncated`.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KibitzerToolCaps {
    pub read_chars: usize,
    pub grep_matches: usize,
    pub grep_line_chars: usize,
    pub grep_scan_files: usize,
    pub grep_scan_bytes: usize,
    pub grep_scan_ms: i64,
    pub session_entries: usize,
    pub session_entry_chars: usize,
    pub memory_search_results: usize,
    pub memory_read_chars: usize,
}

pub const DEFAULT_KIBITZER_TOOL_CAPS: KibitzerToolCaps = KibitzerToolCaps {
    read_chars: 6000,
    grep_matches: 40,
    grep_line_chars: 200,
    grep_scan_files: 5000,
    grep_scan_bytes: 64 * 1024 * 1024,
    grep_scan_ms: 10_000,
    session_entries: 30,
    session_entry_chars: 600,
    memory_search_results: 8,
    memory_read_chars: 6000,
};

pub fn resolve_tool_caps(overrides: Option<KibitzerToolCaps>) -> KibitzerToolCaps {
    overrides.unwrap_or(DEFAULT_KIBITZER_TOOL_CAPS)
}
