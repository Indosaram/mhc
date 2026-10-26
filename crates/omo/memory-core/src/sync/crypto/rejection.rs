pub type RejectionLog = Box<dyn FnMut(&str)>;

/// Only protocol reasons enter this sink; rejected content is never accepted as a log argument.
#[derive(Default)]
pub struct RejectionCounter {
    count: u64,
    last: String,
    log: Option<RejectionLog>,
}
impl RejectionCounter {
    pub fn count(&self) -> u64 {
        self.count
    }
    pub fn last_event(&self) -> &str {
        &self.last
    }
    pub(crate) fn record(&mut self, reason: &'static str) {
        self.count = self.count.saturating_add(1);
        self.last = format!("sync.batch.rejected {reason}");
        if let Some(log) = &mut self.log {
            log(&self.last);
        }
    }
}
pub fn create_rejection_counter(log: Option<RejectionLog>) -> RejectionCounter {
    RejectionCounter {
        log,
        ..RejectionCounter::default()
    }
}
