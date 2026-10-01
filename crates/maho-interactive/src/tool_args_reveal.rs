//! Port of tool-args-reveal.ts with explicit host timer ticks and component identity.
use crate::streaming_reveal_pacing::{next_step, smooth_fps};
use maho_ai::utils::json_parse::parse_streaming_json;
use serde_json::Value;
use std::collections::BTreeMap;
pub const MIN_TOOL_ARGS_PARSE_DELTA: usize = 64;
struct State {
    component: usize,
    target: String,
    revealed: f64,
    rendered: usize,
    last_tick: f64,
}
pub struct ToolArgsRevealController {
    states: BTreeMap<String, State>,
    pub smooth: bool,
    pub fps: f64,
    pub timer_fps: Option<f64>,
}
fn utf16_prefix(text: &str, units: usize) -> String {
    let mut n = 0;
    let mut end = 0;
    for (i, c) in text.char_indices() {
        if n >= units {
            break;
        }
        n += c.len_utf16();
        end = i + c.len_utf8();
    }
    text[..end].into()
}
impl ToolArgsRevealController {
    pub fn new(smooth: bool, fps: f64) -> Self {
        Self {
            states: BTreeMap::new(),
            smooth,
            fps,
            timer_fps: None,
        }
    }
    pub fn update(
        &mut self,
        id: &str,
        component: usize,
        partial: &str,
        now: f64,
    ) -> (bool, Option<Value>) {
        if !self.smooth || partial.is_empty() {
            self.finish(id);
            return (false, None);
        }
        let count = partial.encode_utf16().count();
        let reset = self
            .states
            .get(id)
            .is_none_or(|s| s.component != component || !partial.starts_with(&s.target));
        let result = if reset {
            self.states.insert(
                id.into(),
                State {
                    component,
                    target: partial.into(),
                    revealed: count as f64,
                    rendered: count,
                    last_tick: now,
                },
            );
            Some(parse_streaming_json(Some(partial)))
        } else {
            if let Some(state) = self.states.get_mut(id) {
                let caught = state.revealed >= state.target.encode_utf16().count() as f64;
                state.target = partial.into();
                if caught && state.revealed < (count as f64) {
                    state.last_tick = now;
                }
            }
            None
        };
        self.sync(now, false);
        (true, result)
    }
    pub fn flush(&mut self, id: &str, exact: Value, now: f64) -> Option<Value> {
        self.states.remove(id)?;
        self.sync(now, false);
        Some(exact)
    }
    pub fn flush_all(&mut self) -> Vec<(String, Value)> {
        let outputs = self
            .states
            .iter()
            .map(|(id, s)| (id.clone(), parse_streaming_json(Some(&s.target))))
            .collect();
        self.stop();
        outputs
    }
    pub fn finish(&mut self, id: &str) {
        self.states.remove(id);
        if !self.has_backlog() {
            self.timer_fps = None;
        }
    }
    pub fn refresh(&mut self, now: f64) -> Vec<(String, Value)> {
        if !self.smooth {
            return self.flush_all();
        }
        self.sync(now, true);
        Vec::new()
    }
    pub fn stop(&mut self) {
        self.states.clear();
        self.timer_fps = None;
    }
    fn has_backlog(&self) -> bool {
        self.states
            .values()
            .any(|s| s.revealed < (s.target.encode_utf16().count() as f64))
    }
    fn sync(&mut self, now: f64, restart: bool) {
        if !self.has_backlog() {
            self.timer_fps = None;
            return;
        }
        let fps = smooth_fps(self.fps);
        if restart || self.timer_fps != Some(fps) {
            self.timer_fps = Some(fps);
            for s in self.states.values_mut() {
                if s.revealed < (s.target.encode_utf16().count() as f64) {
                    s.last_tick = now;
                }
            }
        }
    }
    pub fn tick(&mut self, now: f64) -> Vec<(String, Value)> {
        if !self.smooth {
            return self.flush_all();
        }
        let mut outputs = Vec::new();
        for (id, s) in &mut self.states {
            let total = s.target.encode_utf16().count();
            let backlog = total as f64 - s.revealed;
            if backlog <= 0. {
                continue;
            }
            s.revealed =
                (total as f64).min(s.revealed + next_step(backlog, now - s.last_tick, 90.).max(1.));
            s.last_tick = now;
            if s.revealed - (s.rendered as f64) < MIN_TOOL_ARGS_PARSE_DELTA as f64 {
                continue;
            }
            let partial =
                utf16_prefix(&s.target, total.min(s.rendered + MIN_TOOL_ARGS_PARSE_DELTA));
            s.rendered = partial.encode_utf16().count();
            outputs.push((id.clone(), parse_streaming_json(Some(&partial))));
        }
        self.sync(now, false);
        outputs
    }
}
