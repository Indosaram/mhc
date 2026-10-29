//! Recall opener pool and per-session picker (latest `recall-openers.ts`).
//!
//! The opener is presentation only: a memory read picks one headline opener per session, avoiding
//! an immediate repeat within the same session. Exported API: `RECALL_OPENER_MAX_CHARS`,
//! `DEFAULT_RECALL_OPENER`, `RECALL_OPENERS`, `is_valid_opener`, `RecallOpenerPicker`.

use std::collections::BTreeMap;
use std::sync::Arc;

/// Longest opener the contract accepts, in characters (`RECALL_OPENER_MAX_CHARS`).
pub const RECALL_OPENER_MAX_CHARS: usize = 40;

/// The pool's first entry and the picker's fallback (`DEFAULT_RECALL_OPENER`).
pub const DEFAULT_RECALL_OPENER: &str = "Oh, right \u{2014}";

/// The restored 100-entry opener pool (pin `recall-openers.ts`).
pub const RECALL_OPENERS: [&str; 100] = [
    DEFAULT_RECALL_OPENER,
    "Right \u{2014}",
    "Ah, right \u{2014}",
    "Oh, that's right \u{2014}",
    "Ah, that's right \u{2014}",
    "Oh yes \u{2014}",
    "Ah yes \u{2014}",
    "Oh, wait, right \u{2014}",
    "Wait, right \u{2014}",
    "Oh, of course \u{2014}",
    "Of course \u{2014}",
    "Ah, of course \u{2014}",
    "Right, of course \u{2014}",
    "Wait \u{2014}",
    "Oh wait \u{2014}",
    "Hold on \u{2014}",
    "Oh, hold on \u{2014}",
    "Hang on \u{2014}",
    "Oh, hang on \u{2014}",
    "Hang on a second \u{2014}",
    "Wait a second \u{2014}",
    "Hold that thought \u{2014}",
    "Actually, hold on \u{2014}",
    "Actually, wait \u{2014}",
    "Hmm, wait \u{2014}",
    "That reminds me \u{2014}",
    "This reminds me of something \u{2014}",
    "Come to think of it \u{2014}",
    "Now that I think about it \u{2014}",
    "It just came back to me \u{2014}",
    "This just came back to me \u{2014}",
    "Something just came back to me \u{2014}",
    "It's coming back to me \u{2014}",
    "It's all coming back now \u{2014}",
    "It just occurred to me \u{2014}",
    "Something just occurred to me \u{2014}",
    "This rings a bell \u{2014}",
    "Wait, this rings a bell \u{2014}",
    "That rings a bell \u{2014}",
    "This sounds familiar \u{2014}",
    "Wait, this is familiar \u{2014}",
    "I've seen this before \u{2014}",
    "Oh, I've seen this before \u{2014}",
    "I've been here before \u{2014}",
    "We've been here before \u{2014}",
    "This came up before \u{2014}",
    "This has come up before \u{2014}",
    "Oh, this came up once \u{2014}",
    "This one came up before \u{2014}",
    "Oh, this again \u{2014}",
    "Oh, this one \u{2014}",
    "Right, this one \u{2014}",
    "Oh, that one \u{2014}",
    "Ah, that one \u{2014}",
    "I know this one \u{2014}",
    "Oh, I know this one \u{2014}",
    "Hold on, I know this one \u{2014}",
    "Wait, I know this \u{2014}",
    "Oh, I know this \u{2014}",
    "Ah, I know this \u{2014}",
    "Oh, I made a note of this \u{2014}",
    "I noted this once \u{2014}",
    "There's a note on this \u{2014}",
    "Oh, there's a note on this \u{2014}",
    "I have a note on this \u{2014}",
    "I wrote this down once \u{2014}",
    "Oh, I wrote this down \u{2014}",
    "This is in my notes \u{2014}",
    "My notes say \u{2014}",
    "I kept a note on this \u{2014}",
    "I've got this on file \u{2014}",
    "Oh, I have this on file \u{2014}",
    "If I recall \u{2014}",
    "If I recall correctly \u{2014}",
    "As I recall \u{2014}",
    "If memory serves \u{2014}",
    "From what I recall \u{2014}",
    "I seem to recall \u{2014}",
    "I figured this out once \u{2014}",
    "Oh, I'd figured this out before \u{2014}",
    "I've worked this out before \u{2014}",
    "I've been through this \u{2014}",
    "Been through this before \u{2014}",
    "I ran into this before \u{2014}",
    "I've run into this before \u{2014}",
    "Oh, I hit this before \u{2014}",
    "I looked into this once \u{2014}",
    "Oh, I looked this up before \u{2014}",
    "Oh, there it is \u{2014}",
    "Ah, there it is \u{2014}",
    "There it is \u{2014}",
    "Before I forget \u{2014}",
    "Oh, before I forget \u{2014}",
    "Oh, I almost forgot \u{2014}",
    "Almost forgot \u{2014}",
    "Nearly forgot \u{2014}",
    "Oh, nearly forgot \u{2014}",
    "Almost slipped my mind \u{2014}",
    "This nearly slipped my mind \u{2014}",
    "Oh, this slipped my mind \u{2014}",
];

/// `isValidOpener`: non-empty, within the char cap, not whitespace-only, no control characters.
pub fn is_valid_opener(value: &str) -> bool {
    let length = value.encode_utf16().count();
    if length == 0 || length > RECALL_OPENER_MAX_CHARS {
        return false;
    }
    if value.trim().is_empty() {
        return false;
    }
    !value.chars().any(is_control_character)
}

fn is_control_character(ch: char) -> bool {
    matches!(ch, '\u{0000}'..='\u{001f}' | '\u{007f}'..='\u{009f}')
}

/// Picks one opener per session, never repeating the previous pick for the same session.
pub struct RecallOpenerPicker {
    pool: Vec<String>,
    random: Arc<dyn Fn() -> f64 + Send + Sync>,
    last_by_session: BTreeMap<String, String>,
}

impl RecallOpenerPicker {
    /// `random` and `pool` are the two injected seams of `createRecallOpenerPicker`.
    pub fn new(random: Option<Arc<dyn Fn() -> f64 + Send + Sync>>, pool: Option<Vec<String>>) -> Self {
        Self {
            pool: pool.unwrap_or_else(|| RECALL_OPENERS.iter().map(|opener| (*opener).to_string()).collect()),
            random: random.unwrap_or_else(|| Arc::new(default_random)),
            last_by_session: BTreeMap::new(),
        }
    }

    /// `pick(sessionId)`: clamped index, then a one-step advance off the last pick for that session.
    pub fn pick(&mut self, session_id: &str) -> String {
        let length = self.pool.len();
        let mut index = self.clamped_index();
        if length > 1
            && self.pool.get(index).map(String::as_str) == self.last_by_session.get(session_id).map(String::as_str)
        {
            index = (index + 1) % length;
        }
        let opener = self.pool.get(index).cloned().unwrap_or_else(|| DEFAULT_RECALL_OPENER.to_string());
        self.last_by_session.insert(session_id.to_string(), opener.clone());
        opener
    }

    /// `forget(sessionId)`: drops the anti-repeat memory for a session.
    pub fn forget(&mut self, session_id: &str) {
        self.last_by_session.remove(session_id);
    }

    fn clamped_index(&self) -> usize {
        let length = self.pool.len();
        if length == 0 {
            return 0;
        }
        let raw = ((self.random)() * length as f64).floor();
        if raw.is_finite() { (raw.max(0.0) as usize).min(length - 1) } else { 0 }
    }
}

/// Host randomness for the production picker; tests always inject a fixed `random`.
fn default_random() -> f64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|duration| duration.subsec_nanos()).unwrap_or(0);
    f64::from(nanos) / 1_000_000_000.0
}

#[cfg(test)]
#[path = "recall_openers_tests.rs"]
mod tests;
