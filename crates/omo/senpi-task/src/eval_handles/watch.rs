use super::{
    host::{HandleRef, HandleSnapshot},
    update_queue::UpdateQueue,
};
use std::collections::HashMap;
#[derive(Debug)]
pub struct HandleWatch {
    pub initial: Vec<HandleSnapshot>,
    pub updates: UpdateQueue,
}
impl HandleWatch {
    pub fn offer(
        &mut self,
        reference: HandleRef,
        snapshot: HandleSnapshot,
        latest: &mut HashMap<HandleRef, i64>,
    ) {
        if latest
            .get(&reference)
            .is_some_and(|revision| snapshot.revision <= *revision)
        {
            return;
        }
        latest.insert(reference, snapshot.revision);
        self.updates.push(snapshot);
    }
}
