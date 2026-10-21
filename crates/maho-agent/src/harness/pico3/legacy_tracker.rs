use maho_chord::delta::{self, Change, Draft, Op};
use super::types::{JsonValue, Result};

pub struct LegacyTracker {
    tracker: delta::Tracker,
    change: Option<Change>,
    force_base: bool,
}

pub fn track(initial: JsonValue) -> Result<LegacyTracker> {
    Ok(LegacyTracker { tracker: delta::track(initial)?, change: None, force_base: true })
}

impl LegacyTracker {
    pub fn state(&mut self) -> Result<Draft> {
        let change = self.change.get_or_insert_with(|| self.tracker.begin_change());
        Ok(change.state()?)
    }
    pub fn target(&self) -> JsonValue { self.tracker.value().clone() }
    pub fn dirty(&self) -> bool { self.force_base || self.change.is_some() }
    pub fn flush(&mut self) -> Result<Vec<Op>> {
        let prepared = self.change.take().map(|mut change| change.prepare()).transpose()?;
        if let Some(prepared) = &prepared { self.tracker.adopt(prepared)?; }
        if self.force_base {
            self.force_base = false;
            return Ok(vec![Op::Replace(self.tracker.value().clone())]);
        }
        Ok(prepared.map_or_else(Vec::new, |prepared| prepared.ops().to_vec()))
    }
    pub fn rebase(&mut self) { self.force_base = true; }
}

pub type Tracker = LegacyTracker;

#[cfg(test)]
#[path = "legacy_tracker_tests.rs"]
mod tests;
