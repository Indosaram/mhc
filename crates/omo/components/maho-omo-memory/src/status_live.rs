pub const FRAME_INTERVAL_MS: u64 = 320;
pub const MEMORY_REFLECTING_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
pub trait MemoryFooterUi {
    fn set_status(&mut self, key: &str, text: &str);
}
pub trait MemoryFooterProbe {
    fn fingerprint(&mut self, identity: &str) -> Result<Option<String>, String>;
    fn refresh_segments(&mut self, identity: &str) -> Result<Option<String>, String>;
}
struct ActiveFrame {
    identity: String,
    run_id: String,
    frame_index: usize,
}
/// Footer paint state, keyed by session.
///
/// A reflection run is reserved per identity, so several bound sessions can share one active run.
/// Only the sessions the run reads (its `conversationIds`) may paint its spinner: the state is kept
/// per session, and the ticker paints each session's own UI, so an idle session never shows another
/// session's run.
#[derive(Default)]
pub struct MemoryFooterLive {
    frames: std::collections::BTreeMap<String, ActiveFrame>,
    fingerprints: std::collections::BTreeMap<String, Option<String>>,
    disposed: bool,
}
impl MemoryFooterLive {
    pub fn activate(
        &mut self,
        session: &str,
        identity: &str,
        run_id: &str,
        ui: &mut dyn MemoryFooterUi,
    ) {
        if self.disposed {
            return;
        }
        if let Some(frame) = self.frames.get_mut(session) {
            if frame.run_id == run_id && frame.identity == identity {
                return;
            }
            frame.identity = identity.into();
            frame.run_id = run_id.into();
            frame.frame_index = 0;
            self.publish(session, ui);
            return;
        }
        self.frames.insert(
            session.into(),
            ActiveFrame {
                identity: identity.into(),
                run_id: run_id.into(),
                frame_index: 0,
            },
        );
        self.publish(session, ui);
    }
    pub fn settle(
        &mut self,
        session: &str,
        identity: &str,
        run_id: &str,
        ui: &mut dyn MemoryFooterUi,
        probe: &mut dyn MemoryFooterProbe,
    ) {
        if self.disposed {
            return;
        }
        match self.frames.get(session) {
            // An unscoped spinner (bound while a run was already in flight) settles with any run.
            Some(frame) if frame.run_id == run_id || frame.run_id.is_empty() => {
                self.frames.remove(session);
            }
            Some(_) => return,
            None => {
                self.refresh(session, identity, ui, probe);
                return;
            }
        }
        self.fingerprints.remove(session);
        self.recompute(session, identity, ui, probe);
    }
    fn publish(&self, session: &str, ui: &mut dyn MemoryFooterUi) {
        if self.disposed {
            return;
        }
        if let Some(frame) = self.frames.get(session) {
            ui.set_status(
                crate::status::MEMORY_STATUS_KEY,
                &format!(
                    "mem:{} {} reflecting",
                    frame.identity, MEMORY_REFLECTING_FRAMES[frame.frame_index]
                ),
            );
        }
    }
    /// Reflects a bind-time in-flight run: the session paints a spinner until a launch names it.
    pub fn set_active(
        &mut self,
        session: &str,
        identity: &str,
        active: bool,
        ui: &mut dyn MemoryFooterUi,
        probe: &mut dyn MemoryFooterProbe,
    ) {
        if self.disposed {
            return;
        }
        if active {
            if !self.frames.contains_key(session) {
                self.frames.insert(
                    session.into(),
                    ActiveFrame {
                        identity: identity.into(),
                        run_id: String::new(),
                        frame_index: 0,
                    },
                );
            }
            self.publish(session, ui);
            return;
        }
        match self.frames.get(session) {
            Some(frame) if frame.run_id.is_empty() => {
                self.frames.remove(session);
                self.fingerprints.remove(session);
                self.recompute(session, identity, ui, probe);
            }
            Some(_) => {}
            None => self.recompute(session, identity, ui, probe),
        }
    }
    pub fn tick_session(&mut self, session: &str, ui: &mut dyn MemoryFooterUi) {
        if self.disposed {
            return;
        }
        let Some(frame) = self.frames.get_mut(session) else {
            return;
        };
        frame.frame_index = (frame.frame_index + 1) % MEMORY_REFLECTING_FRAMES.len();
        self.publish(session, ui);
    }
    fn recompute(
        &mut self,
        session: &str,
        identity: &str,
        ui: &mut dyn MemoryFooterUi,
        probe: &mut dyn MemoryFooterProbe,
    ) {
        let Ok(fingerprint) = probe.fingerprint(identity) else {
            return;
        };
        if fingerprint.is_some() && self.fingerprints.get(session) == Some(&fingerprint) {
            return;
        }
        let Ok(text) = probe.refresh_segments(identity) else {
            return;
        };
        self.fingerprints.insert(session.into(), fingerprint);
        if !self.disposed
            && !self.frames.contains_key(session)
            && let Some(text) = text
        {
            ui.set_status(crate::status::MEMORY_STATUS_KEY, &text);
        }
    }
    pub fn refresh(
        &mut self,
        session: &str,
        identity: &str,
        ui: &mut dyn MemoryFooterUi,
        probe: &mut dyn MemoryFooterProbe,
    ) {
        if self.disposed || self.frames.contains_key(session) {
            return;
        }
        self.recompute(session, identity, ui, probe);
    }
    pub fn animating_sessions(&self) -> Vec<String> {
        if self.disposed {
            return Vec::new();
        }
        self.frames.keys().cloned().collect()
    }
    pub fn stop_session(&mut self, session: &str) {
        self.frames.remove(session);
        self.fingerprints.remove(session);
    }
    pub fn stop(&mut self) {
        self.frames.clear();
        self.fingerprints.clear();
    }
    pub fn dispose(&mut self) {
        self.disposed = true;
        self.stop();
    }
    pub fn is_animating_session(&self, session: &str) -> bool {
        !self.disposed && self.frames.contains_key(session)
    }
    pub fn is_animating(&self) -> bool {
        !self.disposed && !self.frames.is_empty()
    }
    pub fn is_disposed(&self) -> bool {
        self.disposed
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct Ui(Vec<String>);
    impl MemoryFooterUi for Ui {
        fn set_status(&mut self, key: &str, text: &str) {
            assert_eq!(key, "memory");
            self.0.push(text.into());
        }
    }
    #[derive(Default)]
    struct Probe {
        fingerprint: Option<String>,
        calls: usize,
        fail: bool,
    }
    impl MemoryFooterProbe for Probe {
        fn fingerprint(&mut self, _: &str) -> Result<Option<String>, String> {
            if self.fail {
                Err("git".into())
            } else {
                Ok(self.fingerprint.clone())
            }
        }
        fn refresh_segments(&mut self, id: &str) -> Result<Option<String>, String> {
            self.calls += 1;
            Ok(Some(format!("mem:{id} 2h ago* (+4)")))
        }
    }
    #[test]
    fn animation_cadence_is_320_milliseconds() {
        assert_eq!(FRAME_INTERVAL_MS, 320);
    }
    #[test]
    fn activate_and_settle_publish_and_restore() {
        let mut live = MemoryFooterLive::default();
        let mut ui = Ui::default();
        let mut probe = Probe::default();
        live.activate("session", "agent", "run", &mut ui);
        assert_eq!(ui.0, ["mem:agent ⠋ reflecting"]);
        live.settle("session", "agent", "run", &mut ui, &mut probe);
        assert_eq!(ui.0.last().unwrap(), "mem:agent 2h ago* (+4)");
        assert!(!live.is_animating());
    }
    #[test]
    fn repeated_activation_preserves_current_frame() {
        let mut live = MemoryFooterLive::default();
        let mut ui = Ui::default();
        live.activate("session", "agent", "run", &mut ui);
        live.tick_session("session", &mut ui);
        live.activate("session", "agent", "run", &mut ui);
        assert_eq!(ui.0.len(), 2);
        live.tick_session("session", &mut ui);
        assert_eq!(ui.0[2], "mem:agent ⠹ reflecting");
    }
    #[test]
    fn changed_head_invalidates_cached_footer() {
        let mut live = MemoryFooterLive::default();
        let mut ui = Ui::default();
        let mut probe = Probe {
            fingerprint: Some("old".into()),
            ..Default::default()
        };
        live.refresh("session", "agent", &mut ui, &mut probe);
        probe.fingerprint = Some("new".into());
        live.refresh("session", "agent", &mut ui, &mut probe);
        assert_eq!(probe.calls, 2);
        assert_eq!(ui.0.len(), 2);
    }
    #[test]
    fn active_spinner_is_not_overwritten_by_refresh() {
        let mut live = MemoryFooterLive::default();
        let mut ui = Ui::default();
        let mut probe = Probe::default();
        live.activate("session", "agent", "run", &mut ui);
        live.refresh("session", "agent", &mut ui, &mut probe);
        assert_eq!(probe.calls, 0);
        assert_eq!(ui.0, ["mem:agent ⠋ reflecting"]);
    }
    #[test]
    fn probe_failure_emits_no_status() {
        let mut live = MemoryFooterLive::default();
        let mut ui = Ui::default();
        let mut probe = Probe {
            fail: true,
            ..Default::default()
        };
        live.refresh("session", "agent", &mut ui, &mut probe);
        assert!(ui.0.is_empty());
        assert_eq!(probe.calls, 0);
    }
    #[test]
    fn frames_wrap_and_settle_restores() {
        let mut live = MemoryFooterLive::default();
        let mut ui = Ui::default();
        let mut probe = Probe::default();
        live.activate("session", "agent", "run", &mut ui);
        for _ in 0..10 {
            live.tick_session("session", &mut ui);
        }
        assert_eq!(
            &ui.0[..3],
            [
                "mem:agent ⠋ reflecting",
                "mem:agent ⠙ reflecting",
                "mem:agent ⠹ reflecting"
            ]
        );
        assert_eq!(ui.0[10], ui.0[0]);
        assert!(live.is_animating_session("session"));
        live.settle("session", "agent", "run", &mut ui, &mut probe);
        assert!(!live.is_animating());
        assert_eq!(ui.0.last().unwrap(), "mem:agent 2h ago* (+4)");
    }
    #[test]
    fn stable_probe_skips_and_restore_bypasses_dedupe() {
        let mut live = MemoryFooterLive::default();
        let mut ui = Ui::default();
        let mut probe = Probe {
            fingerprint: Some("sha".into()),
            ..Default::default()
        };
        for _ in 0..2 {
            live.refresh("session", "agent", &mut ui, &mut probe);
        }
        assert_eq!(probe.calls, 1);
        live.activate("session", "agent", "run", &mut ui);
        live.refresh("session", "agent", &mut ui, &mut probe);
        assert_eq!(probe.calls, 1);
        live.settle("session", "agent", "run", &mut ui, &mut probe);
        assert_eq!(probe.calls, 2);
    }
    #[test]
    fn missing_probe_always_refreshes_and_errors_are_silent() {
        let mut live = MemoryFooterLive::default();
        let mut ui = Ui::default();
        let mut probe = Probe::default();
        for _ in 0..2 {
            live.refresh("session", "agent", &mut ui, &mut probe);
        }
        assert_eq!(probe.calls, 2);
        probe.fail = true;
        live.refresh("session", "agent", &mut ui, &mut probe);
        assert_eq!(ui.0.len(), 2);
    }
    #[test]
    fn one_session_never_paints_another_sessions_spinner() {
        let mut live = MemoryFooterLive::default();
        let mut first = Ui::default();
        let mut second = Ui::default();
        live.activate("one", "agent", "run", &mut first);
        assert_eq!(live.animating_sessions(), ["one"]);
        live.tick_session("two", &mut second);
        live.activate("two", "agent", "other-run", &mut second);
        live.tick_session("one", &mut first);
        assert_eq!(
            first.0,
            ["mem:agent ⠋ reflecting", "mem:agent ⠙ reflecting"]
        );
        assert_eq!(second.0, ["mem:agent ⠋ reflecting"]);
        live.settle("one", "agent", "run", &mut first, &mut Probe::default());
        assert_eq!(live.animating_sessions(), ["two"]);
        assert_eq!(second.0, ["mem:agent ⠋ reflecting"]);
    }
    #[test]
    fn settle_of_another_run_keeps_the_spinner() {
        let mut live = MemoryFooterLive::default();
        let mut ui = Ui::default();
        let mut probe = Probe::default();
        live.activate("session", "agent", "run-b", &mut ui);
        live.settle("session", "agent", "run-a", &mut ui, &mut probe);
        assert!(live.is_animating_session("session"));
        assert_eq!(ui.0, ["mem:agent ⠋ reflecting"]);
        assert_eq!(probe.calls, 0);
    }
    #[test]
    fn settle_without_a_spinner_still_refreshes_the_line() {
        let mut live = MemoryFooterLive::default();
        let mut ui = Ui::default();
        let mut probe = Probe::default();
        live.settle("session", "agent", "run", &mut ui, &mut probe);
        assert_eq!(ui.0, ["mem:agent 2h ago* (+4)"]);
    }
    #[test]
    fn stop_session_leaves_other_sessions_animating() {
        let mut live = MemoryFooterLive::default();
        let mut first = Ui::default();
        let mut second = Ui::default();
        live.activate("one", "agent", "run", &mut first);
        live.activate("two", "agent", "run", &mut second);
        live.stop_session("one");
        assert_eq!(live.animating_sessions(), ["two"]);
        live.tick_session("one", &mut first);
        assert_eq!(first.0, ["mem:agent ⠋ reflecting"]);
    }
    #[test]
    fn disposed_footer_ignores_activity_and_refresh() {
        let mut live = MemoryFooterLive::default();
        let mut ui = Ui::default();
        let mut probe = Probe::default();
        live.dispose();
        live.activate("session", "agent", "run", &mut ui);
        live.refresh("session", "agent", &mut ui, &mut probe);
        live.tick_session("session", &mut ui);
        assert!(ui.0.is_empty());
        assert_eq!(probe.calls, 0);
        assert!(live.animating_sessions().is_empty());
    }
    #[test]
    fn stop_allows_restart_dispose_is_terminal() {
        let mut live = MemoryFooterLive::default();
        let mut ui = Ui::default();
        let mut probe = Probe::default();
        live.activate("session", "agent", "run", &mut ui);
        live.stop();
        live.tick_session("session", &mut ui);
        assert_eq!(ui.0.len(), 1);
        live.activate("session", "agent", "run", &mut ui);
        assert_eq!(ui.0.len(), 2);
        live.dispose();
        live.tick_session("session", &mut ui);
        live.activate("session", "agent", "run", &mut ui);
        live.refresh("session", "agent", &mut ui, &mut probe);
        assert_eq!(ui.0.len(), 2);
    }
}
