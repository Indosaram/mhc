#[derive(Default)]
pub struct LoopBlockedTime { blocked_ms_total:f64 }
impl LoopBlockedTime {
    pub fn record_loop_blocked_ms(&mut self,drift_ms:f64) { if drift_ms > 0.0 { self.blocked_ms_total += drift_ms; } }
    pub const fn loop_blocked_mark(&self) -> f64 { self.blocked_ms_total }
    pub fn loop_blocked_ms_since(&self,mark:f64) -> f64 { self.blocked_ms_total - mark }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn only_positive_drift_counts_since_mark() {
        let mut ledger = LoopBlockedTime::default();
        ledger.record_loop_blocked_ms(10.0);
        let mark = ledger.loop_blocked_mark();
        ledger.record_loop_blocked_ms(-5.0); ledger.record_loop_blocked_ms(f64::NAN); ledger.record_loop_blocked_ms(0.0); ledger.record_loop_blocked_ms(25.0);
        assert!((ledger.loop_blocked_ms_since(mark)-25.0).abs() < f64::EPSILON);
    }
}
