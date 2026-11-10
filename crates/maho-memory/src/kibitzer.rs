//! Kibitzer delivery: holding passive nudges until a session can read them.
//!
//! Behaviour spec: `docs/cleanroom/specs/memory-kibitzer.md`. A nudge is held per memory path, so
//! a later nudge for the same path replaces the earlier one. Delivery happens at a tool-result
//! boundary (steer), at an idle wake, or in the next prompt block, and a nudge is never delivered
//! through two routes.
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

/// A short reminder pointing at one stored memory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Nudge {
    /// The memory path the nudge points at.
    pub path: String,
    /// The reminder text.
    pub content: String,
}

impl Nudge {
    /// A nudge pointing at one path.
    pub fn new(path: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            content: content.into(),
        }
    }
}

/// How a nudge reached the session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryVia {
    /// A hidden steer message between tool calls.
    Steer,
    /// A wake that started a turn.
    Wake,
    /// The next prompt block.
    Prompt,
}

impl DeliveryVia {
    /// The label recorded on a delivered entry.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Steer => "steer",
            Self::Wake => "wake",
            Self::Prompt => "prompt",
        }
    }
}

/// The tool-result gate: the session must be running but between turns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ToolResultGate {
    /// Whether the session still has queued messages.
    pub has_pending_messages: bool,
    /// Whether the session is idle.
    pub is_idle: bool,
}

impl ToolResultGate {
    /// Whether a steer may be sent at this boundary.
    pub fn admits_steer(self) -> bool {
        !self.has_pending_messages && !self.is_idle
    }
}

/// Why a steer could not be sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SteerRefusal {
    /// No nudge is held for the session.
    NothingHeld,
    /// A steer is already in flight.
    AlreadySteering,
    /// The session still has queued messages.
    PendingMessages,
    /// The session is idle, so a steer would not be read.
    Idle,
}

/// What one delivery attempt did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeliveryOutcome {
    /// Nothing was delivered.
    None,
    /// These nudges were delivered through the given route.
    Delivered {
        /// The route used.
        via: DeliveryVia,
        /// The nudges handed over, in path order.
        nudges: Vec<Nudge>,
    },
}

#[derive(Default)]
struct SessionState {
    nudges: BTreeMap<String, Nudge>,
    steering: bool,
    tools_in_flight: BTreeMap<String, usize>,
}

impl SessionState {
    fn held(&self) -> Vec<Nudge> {
        self.nudges.values().cloned().collect()
    }
}

/// One session's held nudges and delivery flags.
struct Session {
    state: Mutex<SessionState>,
}

impl Session {
    fn new() -> Self {
        Self {
            state: Mutex::new(SessionState::default()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, SessionState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// The kibitzer's per-session delivery state.
#[derive(Default)]
pub struct Kibitzer {
    sessions: Mutex<BTreeMap<String, Arc<Session>>>,
}

impl Kibitzer {
    /// An empty kibitzer with no sessions.
    pub fn new() -> Self {
        Self::default()
    }

    fn session(&self, session_id: &str) -> Arc<Session> {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        sessions
            .entry(session_id.to_owned())
            .or_insert_with(|| Arc::new(Session::new()))
            .clone()
    }

    fn existing(&self, session_id: &str) -> Option<Arc<Session>> {
        let sessions = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        sessions.get(session_id).cloned()
    }

    /// Hold a batch of nudges for a session, replacing any nudge for the same path.
    ///
    /// Returns how many nudges are held afterwards. An empty batch changes nothing.
    pub fn accept(&self, session_id: &str, nudges: &[Nudge]) -> usize {
        if nudges.is_empty() {
            return self.held_count(session_id);
        }
        let session = self.session(session_id);
        let mut state = session.lock();
        for nudge in nudges {
            state.nudges.insert(nudge.path.clone(), nudge.clone());
        }
        state.nudges.len()
    }

    /// How many nudges a session currently holds.
    pub fn held_count(&self, session_id: &str) -> usize {
        self.existing(session_id)
            .map(|session| session.lock().nudges.len())
            .unwrap_or(0)
    }

    /// Whether a session currently holds a nudge for a path.
    pub fn holds(&self, session_id: &str, path: &str) -> bool {
        self.existing(session_id)
            .map(|session| session.lock().nudges.contains_key(path))
            .unwrap_or(false)
    }

    /// Take every held nudge for the next prompt block, clearing the held map.
    pub fn drain_for_prompt(&self, session_id: &str) -> Vec<Nudge> {
        let Some(session) = self.existing(session_id) else {
            return Vec::new();
        };
        let mut state = session.lock();
        let drained = state.held();
        state.nudges.clear();
        drained
    }

    /// Try to steer at a tool-result boundary.
    ///
    /// The steering flag is set before the nudges are handed back, so a concurrent boundary cannot
    /// send them again. On refusal the nudges stay held.
    pub fn steer(
        &self,
        session_id: &str,
        gate: ToolResultGate,
    ) -> Result<DeliveryOutcome, SteerRefusal> {
        let Some(session) = self.existing(session_id) else {
            return Err(SteerRefusal::NothingHeld);
        };
        let mut state = session.lock();
        if state.nudges.is_empty() {
            return Err(SteerRefusal::NothingHeld);
        }
        if state.steering {
            return Err(SteerRefusal::AlreadySteering);
        }
        if gate.has_pending_messages {
            return Err(SteerRefusal::PendingMessages);
        }
        if gate.is_idle {
            return Err(SteerRefusal::Idle);
        }
        state.steering = true;
        Ok(DeliveryOutcome::Delivered {
            via: DeliveryVia::Steer,
            nudges: state.held(),
        })
    }

    /// Mark the in-flight steer as delivered, clearing the flag and the nudges it carried.
    pub fn mark_delivered(&self, session_id: &str, nudges: &[Nudge]) -> bool {
        let Some(session) = self.existing(session_id) else {
            return false;
        };
        let mut state = session.lock();
        let was_steering = state.steering;
        state.steering = false;
        for nudge in nudges {
            state.nudges.remove(&nudge.path);
        }
        was_steering
    }

    /// Mark a steer as failed to send, keeping the nudges so a later boundary can retry.
    pub fn mark_steer_failed(&self, session_id: &str) {
        if let Some(session) = self.existing(session_id) {
            session.lock().steering = false;
        }
    }

    /// Whether a steer is in flight for a session.
    pub fn is_steering(&self, session_id: &str) -> bool {
        self.existing(session_id)
            .map(|session| session.lock().steering)
            .unwrap_or(false)
    }

    /// Record a tool call starting, so its result boundary can be recognised.
    pub fn mark_tool_started(&self, session_id: &str, tool_call_id: &str) {
        let session = self.session(session_id);
        let mut state = session.lock();
        *state
            .tools_in_flight
            .entry(tool_call_id.to_owned())
            .or_insert(0) += 1;
    }

    /// Record a tool call finishing, returning whether it was in flight.
    pub fn mark_tool_finished(&self, session_id: &str, tool_call_id: &str) -> bool {
        let Some(session) = self.existing(session_id) else {
            return false;
        };
        let mut state = session.lock();
        let Some(count) = state.tools_in_flight.get_mut(tool_call_id) else {
            return false;
        };
        *count = count.saturating_sub(1);
        let finished = *count == 0;
        if finished {
            state.tools_in_flight.remove(tool_call_id);
        }
        finished
    }

    /// How many tool calls a session has in flight.
    pub fn tools_in_flight(&self, session_id: &str) -> usize {
        self.existing(session_id)
            .map(|session| session.lock().tools_in_flight.len())
            .unwrap_or(0)
    }

    /// Drop a session's state on shutdown; nothing is delivered after this.
    pub fn on_session_shutdown(&self, session_id: &str) -> usize {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        sessions
            .remove(session_id)
            .map(|session| session.lock().nudges.len())
            .unwrap_or(0)
    }

    /// How many sessions currently hold state.
    pub fn session_count(&self) -> usize {
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUNNING: ToolResultGate = ToolResultGate {
        has_pending_messages: false,
        is_idle: false,
    };

    fn nudge(path: &str) -> Nudge {
        Nudge::new(path, format!("reminder for {path}"))
    }

    #[test]
    fn an_accepted_nudge_is_held_for_its_session() {
        let kibitzer = Kibitzer::new();
        assert_eq!(kibitzer.accept("s1", &[nudge("notes/a.md")]), 1);
        assert!(kibitzer.holds("s1", "notes/a.md"));
        assert_eq!(kibitzer.held_count("s1"), 1);
    }

    #[test]
    fn a_later_nudge_for_the_same_path_replaces_the_earlier_one() {
        let kibitzer = Kibitzer::new();
        kibitzer.accept("s1", &[Nudge::new("notes/a.md", "first")]);
        kibitzer.accept("s1", &[Nudge::new("notes/a.md", "second")]);
        assert_eq!(kibitzer.held_count("s1"), 1);
        let drained = kibitzer.drain_for_prompt("s1");
        assert_eq!(drained[0].content, "second");
    }

    #[test]
    fn an_empty_batch_holds_nothing() {
        let kibitzer = Kibitzer::new();
        assert_eq!(kibitzer.accept("s1", &[]), 0);
        assert_eq!(kibitzer.session_count(), 0);
    }

    #[test]
    fn sessions_do_not_see_each_others_nudges() {
        let kibitzer = Kibitzer::new();
        kibitzer.accept("s1", &[nudge("notes/a.md")]);
        assert!(!kibitzer.holds("s2", "notes/a.md"));
        assert_eq!(kibitzer.held_count("s2"), 0);
    }

    #[test]
    fn draining_for_a_prompt_empties_the_held_map() {
        let kibitzer = Kibitzer::new();
        kibitzer.accept("s1", &[nudge("a"), nudge("b")]);
        let drained = kibitzer.drain_for_prompt("s1");
        assert_eq!(drained.len(), 2);
        assert_eq!(kibitzer.held_count("s1"), 0);
        assert!(kibitzer.drain_for_prompt("s1").is_empty());
    }

    #[test]
    fn a_steer_at_a_running_boundary_delivers_every_held_nudge() {
        let kibitzer = Kibitzer::new();
        kibitzer.accept("s1", &[nudge("a"), nudge("b")]);
        let outcome = kibitzer.steer("s1", RUNNING).expect("steer");
        match outcome {
            DeliveryOutcome::Delivered { via, nudges } => {
                assert_eq!(via, DeliveryVia::Steer);
                assert_eq!(nudges.len(), 2);
            }
            DeliveryOutcome::None => panic!("a steer should deliver"),
        }
        assert!(kibitzer.is_steering("s1"));
    }

    #[test]
    fn a_second_boundary_cannot_steer_while_the_first_is_in_flight() {
        let kibitzer = Kibitzer::new();
        kibitzer.accept("s1", &[nudge("a")]);
        assert!(kibitzer.steer("s1", RUNNING).is_ok());
        assert_eq!(
            kibitzer.steer("s1", RUNNING),
            Err(SteerRefusal::AlreadySteering)
        );
    }

    #[test]
    fn a_steer_is_refused_while_messages_are_pending() {
        let kibitzer = Kibitzer::new();
        kibitzer.accept("s1", &[nudge("a")]);
        let gate = ToolResultGate {
            has_pending_messages: true,
            is_idle: false,
        };
        assert_eq!(
            kibitzer.steer("s1", gate),
            Err(SteerRefusal::PendingMessages)
        );
        assert!(!kibitzer.is_steering("s1"));
        assert_eq!(kibitzer.held_count("s1"), 1);
    }

    #[test]
    fn a_steer_is_refused_when_the_session_is_idle() {
        let kibitzer = Kibitzer::new();
        kibitzer.accept("s1", &[nudge("a")]);
        let gate = ToolResultGate {
            has_pending_messages: false,
            is_idle: true,
        };
        assert_eq!(kibitzer.steer("s1", gate), Err(SteerRefusal::Idle));
    }

    #[test]
    fn a_steer_with_nothing_held_is_refused() {
        let kibitzer = Kibitzer::new();
        assert_eq!(
            kibitzer.steer("s1", RUNNING),
            Err(SteerRefusal::NothingHeld)
        );
    }

    #[test]
    fn marking_delivered_clears_the_flag_and_the_delivered_nudges() {
        let kibitzer = Kibitzer::new();
        kibitzer.accept("s1", &[nudge("a"), nudge("b")]);
        let delivered = match kibitzer.steer("s1", RUNNING).expect("steer") {
            DeliveryOutcome::Delivered { nudges, .. } => nudges,
            DeliveryOutcome::None => panic!("a steer should deliver"),
        };
        assert!(kibitzer.mark_delivered("s1", &delivered));
        assert!(!kibitzer.is_steering("s1"));
        assert_eq!(kibitzer.held_count("s1"), 0);
    }

    #[test]
    fn a_nudge_accepted_while_a_steer_is_in_flight_survives_delivery() {
        let kibitzer = Kibitzer::new();
        kibitzer.accept("s1", &[nudge("a")]);
        let delivered = match kibitzer.steer("s1", RUNNING).expect("steer") {
            DeliveryOutcome::Delivered { nudges, .. } => nudges,
            DeliveryOutcome::None => panic!("a steer should deliver"),
        };
        kibitzer.accept("s1", &[nudge("b")]);
        kibitzer.mark_delivered("s1", &delivered);
        assert!(kibitzer.holds("s1", "b"));
        assert!(!kibitzer.holds("s1", "a"));
    }

    #[test]
    fn a_failed_steer_keeps_the_nudges_and_clears_the_flag() {
        let kibitzer = Kibitzer::new();
        kibitzer.accept("s1", &[nudge("a")]);
        assert!(kibitzer.steer("s1", RUNNING).is_ok());
        kibitzer.mark_steer_failed("s1");
        assert!(!kibitzer.is_steering("s1"));
        assert_eq!(kibitzer.held_count("s1"), 1);
        assert!(kibitzer.steer("s1", RUNNING).is_ok());
    }

    #[test]
    fn a_tool_call_is_in_flight_until_its_last_finish() {
        let kibitzer = Kibitzer::new();
        kibitzer.mark_tool_started("s1", "call-1");
        assert_eq!(kibitzer.tools_in_flight("s1"), 1);
        kibitzer.mark_tool_started("s1", "call-1");
        assert!(!kibitzer.mark_tool_finished("s1", "call-1"));
        assert!(kibitzer.mark_tool_finished("s1", "call-1"));
        assert_eq!(kibitzer.tools_in_flight("s1"), 0);
        assert!(!kibitzer.mark_tool_finished("s1", "call-1"));
    }

    #[test]
    fn shutdown_drops_the_session_and_nothing_is_delivered_afterwards() {
        let kibitzer = Kibitzer::new();
        kibitzer.accept("s1", &[nudge("a")]);
        assert_eq!(kibitzer.on_session_shutdown("s1"), 1);
        assert_eq!(kibitzer.session_count(), 0);
        assert_eq!(
            kibitzer.steer("s1", RUNNING),
            Err(SteerRefusal::NothingHeld)
        );
        assert!(kibitzer.drain_for_prompt("s1").is_empty());
    }

    #[test]
    fn the_gate_admits_only_a_running_session_between_turns() {
        assert!(RUNNING.admits_steer());
        assert!(
            !ToolResultGate {
                has_pending_messages: true,
                is_idle: false
            }
            .admits_steer()
        );
        assert!(
            !ToolResultGate {
                has_pending_messages: false,
                is_idle: true
            }
            .admits_steer()
        );
    }

    #[test]
    fn a_delivery_route_renders_as_its_recorded_label() {
        assert_eq!(DeliveryVia::Steer.as_str(), "steer");
        assert_eq!(DeliveryVia::Wake.as_str(), "wake");
        assert_eq!(DeliveryVia::Prompt.as_str(), "prompt");
    }
}
