use crate::bridge::protocol::{KernelMemoryGlobal, KernelMemoryReport, KernelMemoryThresholds};

#[derive(Clone)]
struct CeilingBreach {
    live_bytes: u64,
    globals: Vec<KernelMemoryGlobal>,
}

pub struct KernelMemoryPolicy {
    collects: bool,
    thresholds: KernelMemoryThresholds,
    last_notified_live: u64,
    last_observed_live: u64,
    pending_recycle: Option<CeilingBreach>,
    announce_recycle: Option<CeilingBreach>,
}

impl KernelMemoryPolicy {
    pub fn new(thresholds: KernelMemoryThresholds) -> Self {
        Self {
            collects: true,
            thresholds,
            last_notified_live: 0,
            last_observed_live: 0,
            pending_recycle: None,
            announce_recycle: None,
        }
    }

    pub fn process(thresholds: KernelMemoryThresholds) -> Self {
        Self {
            collects: false,
            ..Self::new(thresholds)
        }
    }

    pub fn observe_live(&mut self, live_bytes: u64) {
        if live_bytes < self.thresholds.notice_bytes / 2 {
            self.last_notified_live = 0;
        }
        self.last_observed_live = live_bytes;
    }

    pub fn annotate(&mut self, mut report: KernelMemoryReport) -> KernelMemoryReport {
        let measure = if self.collects {
            "live after GC"
        } else {
            "in its process footprint"
        };
        let crossed_upward = self.last_observed_live < self.thresholds.notice_bytes;
        self.observe_live(report.live_bytes);
        if let Some(breach) = self.announce_recycle.take() {
            report.recycled = Some(true);
            report.notice = Some(format!(
                "[js kernel was restarted before this cell: {} {measure} exceeded the {} ceiling. Every global from earlier cells is gone.{}]",
                format_bytes(breach.live_bytes),
                format_bytes(self.thresholds.ceiling_bytes),
                globals_phrase("Largest globals were", &breach.globals)
            ));
            return report;
        }
        if self.collects && report.gc_ran != Some(true) {
            return report;
        }
        if self.thresholds.ceiling_bytes > 0 && report.live_bytes >= self.thresholds.ceiling_bytes {
            self.last_notified_live = report.live_bytes;
            report.over_ceiling = Some(true);
            if self.pending_recycle.is_none() {
                let breach = CeilingBreach {
                    live_bytes: report.live_bytes,
                    globals: report.globals.clone().unwrap_or_default(),
                };
                report.notice = Some(format!(
                    "[js kernel holds {} {measure}, over the {} ceiling. It restarts before the next cell you submit and every global is lost.{}]",
                    format_bytes(breach.live_bytes),
                    format_bytes(self.thresholds.ceiling_bytes),
                    globals_phrase("Largest globals", &breach.globals)
                ));
                self.pending_recycle = Some(breach);
            }
            return report;
        }
        if self.thresholds.notice_bytes == 0 || report.live_bytes < self.thresholds.notice_bytes {
            return report;
        }
        let grew = u128::from(report.live_bytes) * 4 >= u128::from(self.last_notified_live) * 5;
        if !crossed_upward && !grew {
            return report;
        }
        self.last_notified_live = report.live_bytes;
        let globals = report.globals.as_deref().unwrap_or_default();
        let drop = globals
            .first()
            .map_or("name", |global| global.name.as_str());
        report.notice = Some(format!(
            "[js kernel holds {} {measure} (notice at {}).{} Drop what you no longer need ({drop} = undefined) or run with reset: true.]",
            format_bytes(report.live_bytes),
            format_bytes(self.thresholds.notice_bytes),
            globals_phrase("Largest globals", globals)
        ));
        report
    }

    pub fn claim_recycle(&mut self, idle: bool) -> bool {
        if !idle || self.pending_recycle.is_none() {
            return false;
        }
        self.announce_recycle = self.pending_recycle.take();
        self.last_notified_live = 0;
        self.last_observed_live = 0;
        true
    }

    pub fn kernel_retired(&mut self) {
        self.pending_recycle = None;
        self.last_notified_live = 0;
        self.last_observed_live = 0;
    }
}

pub fn format_bytes(bytes: u64) -> String {
    const MIB: u64 = 1024 * 1024;
    const GIB: u64 = 1024 * MIB;
    if bytes >= GIB {
        let tenths = (u128::from(bytes) * 10 + u128::from(GIB / 2)) / u128::from(GIB);
        if tenths % 10 == 0 {
            format!("{} GB", tenths / 10)
        } else {
            format!("{}.{} GB", tenths / 10, tenths % 10)
        }
    } else {
        format!("{} MB", ((bytes.saturating_add(MIB / 2)) / MIB).max(1))
    }
}

fn globals_phrase(label: &str, globals: &[KernelMemoryGlobal]) -> String {
    if globals.is_empty() {
        return String::new();
    }
    let listed = globals
        .iter()
        .take(5)
        .map(|global| {
            format!(
                "{} {}{}",
                global.name,
                if global.approximate == Some(true) {
                    "~"
                } else {
                    ""
                },
                format_bytes(global.bytes)
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(" {label}: {listed}.")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> KernelMemoryPolicy {
        KernelMemoryPolicy::new(KernelMemoryThresholds {
            gc_watermark_bytes: 50,
            notice_bytes: 100,
            ceiling_bytes: 200,
        })
    }
    fn report(bytes: u64, collected: bool) -> KernelMemoryReport {
        serde_json::from_value(
            serde_json::json!({"liveBytes":bytes,"measure":"heap","gcRan":collected}),
        )
        .expect("memory report")
    }

    #[test]
    fn notice_requires_collection_and_rearms_after_drop() {
        let mut policy = policy();
        assert!(policy.annotate(report(100, false)).notice.is_none());
        assert!(policy.annotate(report(100, true)).notice.is_some());
        assert!(policy.annotate(report(124, true)).notice.is_none());
        assert!(policy.annotate(report(125, true)).notice.is_some());
        policy.observe_live(49);
        assert!(policy.annotate(report(100, true)).notice.is_some());
    }

    #[test]
    fn ceiling_recycles_only_idle_and_announces_once_after_retirement() {
        let mut policy = policy();
        assert_eq!(policy.annotate(report(200, true)).over_ceiling, Some(true));
        assert!(!policy.claim_recycle(false));
        assert!(policy.claim_recycle(true));
        policy.kernel_retired();
        assert_eq!(policy.annotate(report(10, false)).recycled, Some(true));
        assert!(policy.annotate(report(10, true)).recycled.is_none());
    }

    #[test]
    fn retirement_clears_unclaimed_ceiling() {
        let mut policy = policy();
        policy.annotate(report(200, true));
        policy.kernel_retired();
        assert!(!policy.claim_recycle(true));
    }
}
