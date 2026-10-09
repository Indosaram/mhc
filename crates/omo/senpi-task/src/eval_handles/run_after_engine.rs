use super::host::{HandlePhase, HandleRef};
#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum Verdict { Live, Moved, RolledBack }
#[derive(Debug, Clone, Copy)] pub struct RunAfterEngine { pub observed_epoch: i64, pub expected_epoch: i64, pub verdict: Verdict }
impl RunAfterEngine { pub fn new(observed_epoch: i64, expected_epoch: i64) -> Self { let verdict = if observed_epoch == expected_epoch { Verdict::Live } else if observed_epoch > expected_epoch { Verdict::Moved } else { Verdict::RolledBack }; Self { observed_epoch, expected_epoch, verdict } } pub fn ref_for(&self, reference: &HandleRef) -> HandleRef { HandleRef { run_epoch: self.observed_epoch, ..reference.clone() } } pub fn phase(&self) -> HandlePhase { HandlePhase::Pending } }
