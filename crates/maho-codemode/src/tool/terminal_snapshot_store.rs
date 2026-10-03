use std::collections::VecDeque;
use super::detached_cell_contract::EvalDetachedCellSnapshot;

pub const TERMINAL_SNAPSHOT_CAP: usize = 32;

pub struct TerminalSnapshotStore { snapshots: VecDeque<EvalDetachedCellSnapshot>, cap: usize }

impl Default for TerminalSnapshotStore {
    fn default() -> Self { Self::new(TERMINAL_SNAPSHOT_CAP) }
}

impl TerminalSnapshotStore {
    pub fn new(cap: usize) -> Self { Self { snapshots: VecDeque::new(), cap } }

    pub fn remember(&mut self, snapshot: EvalDetachedCellSnapshot) {
        self.delete(&snapshot.cell_id);
        self.snapshots.push_back(snapshot);
        while self.snapshots.len() > self.cap { self.snapshots.pop_front(); }
    }

    pub fn get(&self, cell_id: &str) -> Option<&EvalDetachedCellSnapshot> { self.snapshots.iter().find(|snapshot| snapshot.cell_id == cell_id) }
    pub fn delete(&mut self, cell_id: &str) { self.snapshots.retain(|snapshot| snapshot.cell_id != cell_id); }
    pub fn list(&self) -> Vec<&EvalDetachedCellSnapshot> { self.snapshots.iter().collect() }
    pub fn clear(&mut self) { self.snapshots.clear(); }
}
