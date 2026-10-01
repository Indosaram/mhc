//! Port of tool-result-reveal.ts; preserves result metadata and non-text blocks.
use crate::streaming_reveal_content::BlockUnitCounter;
use crate::streaming_reveal_pacing::{next_step, smooth_fps};
use serde_json::Value;
use std::collections::BTreeMap;
struct State {
    component: usize,
    target: Value,
    text: String,
    revealed: f64,
    last_tick: f64,
    counter: BlockUnitCounter,
}
fn first_text(result: &Value) -> Option<&str> {
    let b = result.get("content")?.get(0)?;
    if b.get("type")?.as_str()? == "text" {
        b.get("text")?.as_str()
    } else {
        None
    }
}
fn display(s: &mut State, revealed: f64) -> Value {
    let mut result = s.target.clone();
    result["content"][0]["text"] = Value::String(s.counter.slice(
        0,
        &s.text,
        revealed.floor().max(0.) as usize,
    ));
    result["isError"] = Value::Bool(false);
    result
}
pub struct ToolResultRevealController {
    states: BTreeMap<String, State>,
    pub smooth: bool,
    pub fps: f64,
    pub timer_fps: Option<f64>,
}
impl ToolResultRevealController {
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
        result: Value,
        now: f64,
    ) -> (bool, Option<Value>) {
        let text = first_text(&result).map(str::to_owned);
        if !self.smooth || text.is_none() {
            self.states.remove(id);
            self.sync(now, false);
            return (false, None);
        }
        let text = text.unwrap_or_default();
        let reset = self
            .states
            .get(id)
            .is_none_or(|s| s.component != component || !text.starts_with(&s.text));
        let output = if reset {
            let mut state = State {
                component,
                target: result,
                text,
                revealed: 0.,
                last_tick: now,
                counter: BlockUnitCounter::default(),
            };
            state.revealed = state.counter.count(0, &state.text) as f64;
            let r = state.revealed;
            let out = display(&mut state, r);
            self.states.insert(id.into(), state);
            Some(out)
        } else {
            if let Some(s) = self.states.get_mut(id) {
                let caught = s.revealed >= s.counter.count(0, &s.text) as f64;
                s.target = result;
                s.text = text;
                let total = s.counter.count(0, &s.text);
                if caught && s.revealed < (total as f64) {
                    s.last_tick = now;
                }
            }
            None
        };
        self.sync(now, false);
        (true, output)
    }
    pub fn finish(&mut self, id: &str, now: f64) -> Option<Value> {
        let mut s = self.states.remove(id)?;
        let total = s.counter.count(0, &s.text) as f64;
        let result = display(&mut s, total);
        self.sync(now, false);
        Some(result)
    }
    fn flush_all(&mut self) -> Vec<(String, Value)> {
        let states = std::mem::take(&mut self.states);
        let out = states
            .into_iter()
            .map(|(id, mut s)| {
                let total = s.counter.count(0, &s.text) as f64;
                (id, display(&mut s, total))
            })
            .collect();
        self.timer_fps = None;
        out
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
    fn has_backlog(&mut self) -> bool {
        self.states
            .values_mut()
            .any(|s| s.revealed < (s.counter.count(0, &s.text) as f64))
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
                if s.revealed < (s.counter.count(0, &s.text) as f64) {
                    s.last_tick = now;
                }
            }
        }
    }
    pub fn tick(&mut self, now: f64) -> Vec<(String, Value)> {
        if !self.smooth {
            return self.flush_all();
        }
        let mut out = Vec::new();
        for (id, s) in &mut self.states {
            let total = s.counter.count(0, &s.text) as f64;
            let backlog = total - s.revealed;
            if backlog <= 0. {
                continue;
            }
            s.revealed = total.min(s.revealed + next_step(backlog, now - s.last_tick, 90.));
            s.last_tick = now;
            let r = s.revealed;
            out.push((id.clone(), display(s, r)));
        }
        self.sync(now, false);
        out
    }
}
