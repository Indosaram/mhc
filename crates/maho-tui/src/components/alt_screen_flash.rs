//! Port of senpi `packages/tui/src/components/alt-screen-flash.ts`.
//!
//! senpi's per-entry `setTimeout` (unref'd) becomes an explicit `expires_at_ms` deadline;
//! [`AltScreenFlashContainer::tick`] expires due entries and reports whether a render is
//! needed, following the same poll-driven pattern as `ScrollView`'s scrollbar-hide timer.

use crate::tui::Component;
use crate::utils::truncate_to_width;

const DEFAULT_DURATION_MS: u64 = 1000;

struct FlashEntry {
    message: String,
    expires_at_ms: u64,
}

pub struct AltScreenFlashContainer {
    entries: Vec<FlashEntry>,
}

impl AltScreenFlashContainer {
    pub fn new() -> Self {
        Self { entries: Vec::new() }
    }

    pub fn flash(&mut self, message: impl Into<String>, duration_ms: Option<u64>, now_ms: u64) {
        let duration_ms = duration_ms.unwrap_or(DEFAULT_DURATION_MS);
        self.entries.push(FlashEntry {
            message: message.into(),
            expires_at_ms: now_ms + duration_ms,
        });
    }

    /// Expire due entries. Returns whether a render is needed (senpi's `requestRender()`
    /// call inside the expired timer's callback, plus the call made right after `flash()`
    /// pushes a new entry - callers request a render themselves on `flash()` since that push
    /// is synchronous).
    pub fn tick(&mut self, now_ms: u64) -> bool {
        let before = self.entries.len();
        self.entries.retain(|entry| entry.expires_at_ms > now_ms);
        self.entries.len() != before
    }
}

impl Default for AltScreenFlashContainer {
    fn default() -> Self {
        Self::new()
    }
}

impl Component for AltScreenFlashContainer {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.entries
            .iter()
            .map(|entry| {
                let message = truncate_to_width(&format!(" {} ", entry.message), width, "", false);
                format!("\x1b[7m{message}\x1b[27m")
            })
            .collect()
    }

    fn dispose(&mut self) {
        self.entries.clear();
    }
}
