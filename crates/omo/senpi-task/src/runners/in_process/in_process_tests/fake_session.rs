//! A scripted host session: `prompt` blocks until the test releases it, like the TS fakes whose
//! prompt promise settles only when the test calls `resolvePrompt`.

use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;

use serde_json::Value;

use crate::host::HostError;
use crate::manager::child_handle::Unsubscribe;
use crate::runners::in_process::child_handle::{ChildSession, ChildSessionListener};

#[derive(Default)]
struct FakeState {
    prompt_calls: usize,
    releases: usize,
    follow_up_calls: Vec<String>,
    last_text: Option<String>,
    rejections: Vec<(usize, String)>,
    steer_calls: Vec<String>,
    abort_calls: usize,
    listeners: Vec<(usize, ChildSessionListener)>,
    next_listener: usize,
    disposed: usize,
}

pub(super) type OnPrompt = Box<dyn Fn() + Send + Sync>;

#[derive(Default)]
pub(super) struct FakeSession {
    session_id: String,
    immediate: Option<OnPrompt>,
    state: Mutex<FakeState>,
    changed: Condvar,
}

impl FakeSession {
    pub(super) fn new(session_id: &str) -> Arc<Self> {
        Arc::new(Self {
            session_id: session_id.to_string(),
            ..Self::default()
        })
    }

    /// Prompts settle at once with last text "done", after running `on_prompt`.
    pub(super) fn immediate(session_id: &str, on_prompt: Option<OnPrompt>) -> Arc<Self> {
        Arc::new(Self {
            session_id: session_id.to_string(),
            immediate: Some(on_prompt.unwrap_or_else(|| Box::new(|| {}))),
            ..Self::default()
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, FakeState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(super) fn emit(&self, event: &Value) {
        let listeners: Vec<ChildSessionListener> = self
            .lock()
            .listeners
            .iter()
            .map(|(_, listener)| Arc::clone(listener))
            .collect();
        for listener in listeners {
            listener(event);
        }
    }

    pub(super) fn resolve_prompt(&self) {
        self.lock().releases += 1;
        self.changed.notify_all();
    }

    pub(super) fn set_last_text(&self, text: &str) {
        self.lock().last_text = Some(text.to_string());
    }

    pub(super) fn reject_prompt(&self, message: &str) {
        let mut state = self.lock();
        let ticket = state.releases + 1;
        state.rejections.push((ticket, message.to_string()));
        state.releases = ticket;
        drop(state);
        self.changed.notify_all();
    }

    pub(super) fn steer_calls(&self) -> Vec<String> {
        self.lock().steer_calls.clone()
    }

    pub(super) fn abort_calls(&self) -> usize {
        self.lock().abort_calls
    }

    pub(super) fn follow_up_calls(&self) -> Vec<String> {
        self.lock().follow_up_calls.clone()
    }

    pub(super) fn disposed(&self) -> usize {
        self.lock().disposed
    }

    pub(super) fn prompt_calls(&self) -> usize {
        self.lock().prompt_calls
    }

    pub(super) fn wait_prompt_calls(&self, count: usize) {
        let state = self.lock();
        let (state, timeout) = self
            .changed
            .wait_timeout_while(state, Duration::from_secs(5), |state| {
                state.prompt_calls < count
            })
            .unwrap_or_else(PoisonError::into_inner);
        assert!(!timeout.timed_out(), "prompt #{count} never started");
        assert_eq!(state.prompt_calls, count);
    }
}

impl ChildSession for FakeSession {
    fn session_id(&self) -> String {
        self.session_id.clone()
    }

    fn prompt(&self, _text: &str) -> Result<(), HostError> {
        if let Some(on_prompt) = &self.immediate {
            on_prompt();
            let mut state = self.lock();
            state.prompt_calls += 1;
            state.last_text = Some("done".to_string());
            return Ok(());
        }
        let mut state = self.lock();
        state.prompt_calls += 1;
        let ticket = state.prompt_calls;
        self.changed.notify_all();
        let state = self
            .changed
            .wait_while(state, |state| state.releases < ticket)
            .unwrap_or_else(PoisonError::into_inner);
        match state.rejections.iter().find(|(at, _)| *at == ticket) {
            Some((_, message)) => Err(HostError {
                message: message.clone(),
            }),
            None => Ok(()),
        }
    }

    fn steer(&self, text: &str) -> Result<(), HostError> {
        self.lock().steer_calls.push(text.to_string());
        Ok(())
    }

    fn follow_up(&self, text: &str) -> Result<(), HostError> {
        self.lock().follow_up_calls.push(text.to_string());
        Ok(())
    }

    fn abort(&self) -> Result<(), HostError> {
        self.lock().abort_calls += 1;
        Ok(())
    }

    fn subscribe(&self, listener: ChildSessionListener) -> Unsubscribe {
        let mut state = self.lock();
        let id = state.next_listener;
        state.next_listener += 1;
        state.listeners.push((id, listener));
        Box::new(|| {})
    }

    fn get_last_assistant_text(&self) -> Option<String> {
        self.lock().last_text.clone()
    }

    fn dispose(&self) {
        self.lock().disposed += 1;
    }
}
