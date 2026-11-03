//! `runners/rpc/heartbeat.ts`: the stdio child's single-flight state probe.
//!
//! One probe at a time; a state answer or a child session event is evidence of life. Three
//! consecutive misses declare the child unresponsive. The timing decisions live in
//! [`HeartbeatMachine`] so the thread driver in `rpc/handle.rs` stays a thin clock.

pub const HEARTBEAT_DEADLINE_MS: i64 = 10_000;
const UNRESPONSIVE_AFTER: u32 = 3;

pub fn unresponsive_message(task_id: &str) -> String {
    format!(
        "ended: the child stopped answering for 30 s (3 heartbeats). Action: inspect task_output {task_id}; run it again with task"
    )
}

/// What a probe deadline means once activity is taken into account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissVerdict {
    /// The child was active recently: wait this many more ms before counting a miss.
    Rearm(i64),
    /// One miss was counted; wait a full deadline for the next.
    Missed,
    /// The third consecutive failure: the child is unresponsive.
    Unresponsive,
}

#[derive(Debug, Default)]
pub struct HeartbeatMachine {
    failures: u32,
    last_activity: Option<i64>,
    stopped: bool,
}

impl HeartbeatMachine {
    pub fn stopped(&self) -> bool {
        self.stopped
    }

    pub fn failures(&self) -> u32 {
        self.failures
    }

    /// The probe deadline elapsed with no answer.
    pub fn miss(&mut self, now: i64) -> MissVerdict {
        if let Some(last) = self.last_activity {
            let quiet_for = now - last;
            if quiet_for < HEARTBEAT_DEADLINE_MS {
                return MissVerdict::Rearm(HEARTBEAT_DEADLINE_MS - quiet_for);
            }
        }
        self.fail()
    }

    /// The probe was refused. A refusal after a counted miss is the same silence, not a new one.
    pub fn refused(&mut self, already_missed: bool) -> Option<MissVerdict> {
        (!already_missed && !self.stopped).then(|| self.fail())
    }

    pub fn answered(&mut self) {
        if !self.stopped {
            self.failures = 0;
        }
    }

    /// `activity()`: any child session event resets the failures and pushes the deadline out.
    pub fn activity(&mut self, now: i64) {
        if self.stopped {
            return;
        }
        self.failures = 0;
        self.last_activity = Some(now);
    }

    pub fn stop(&mut self) {
        self.stopped = true;
    }

    fn fail(&mut self) -> MissVerdict {
        self.failures += 1;
        if self.failures >= UNRESPONSIVE_AFTER {
            self.stopped = true;
            return MissVerdict::Unresponsive;
        }
        MissVerdict::Missed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_silent_deadlines_declare_the_child_unresponsive() {
        let mut machine = HeartbeatMachine::default();
        assert_eq!(machine.miss(10_000), MissVerdict::Missed);
        assert_eq!(machine.miss(20_000), MissVerdict::Missed);
        assert_eq!(machine.miss(30_000), MissVerdict::Unresponsive);
        assert!(machine.stopped());
        assert!(unresponsive_message("st_1").contains("inspect task_output st_1"));
    }

    /// `heartbeat-activity.test.ts`: live events keep a busy child from being declared unresponsive.
    #[test]
    fn activity_resets_failures_and_pushes_the_deadline_out() {
        let mut machine = HeartbeatMachine::default();
        let mut now = 0;
        for _ in 0..12 {
            now += 9_000;
            machine.activity(now);
            assert_eq!(machine.miss(now + 1_000), MissVerdict::Rearm(9_000));
        }
        assert_eq!(machine.failures(), 0);
        let quiet = now + HEARTBEAT_DEADLINE_MS;
        assert_eq!(machine.miss(quiet), MissVerdict::Missed);
        assert_eq!(machine.miss(quiet + 10_000), MissVerdict::Missed);
        assert_eq!(machine.miss(quiet + 20_000), MissVerdict::Unresponsive);
    }

    #[test]
    fn a_refusal_after_a_counted_miss_is_not_counted_twice() {
        let mut machine = HeartbeatMachine::default();
        assert_eq!(machine.miss(10_000), MissVerdict::Missed);
        assert_eq!(machine.refused(true), None);
        assert_eq!(machine.refused(false), Some(MissVerdict::Missed));
        machine.answered();
        assert_eq!(machine.failures(), 0);
    }
}
