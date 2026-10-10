use std::collections::VecDeque;
use super::host::HandleSnapshot;
#[derive(Debug, Default)] pub struct UpdateQueue { updates: VecDeque<HandleSnapshot>, ended: bool }
impl UpdateQueue { pub fn push(&mut self, snapshot: HandleSnapshot) { if !self.ended { self.updates.push_back(snapshot); } } pub fn end(&mut self) { self.ended = true; } pub fn take_next(&mut self) -> Option<HandleSnapshot> { self.updates.pop_front() } pub fn is_ended(&self) -> bool { self.ended && self.updates.is_empty() } }
