//! Port of streaming-reveal.ts. Tick timestamps come from the host's monotonic timer.
pub use crate::streaming_reveal_content::*;
pub use crate::streaming_reveal_pacing::*;
use serde_json::Value;
#[derive(Default)]
pub struct StreamingRevealController {
    target: Option<Value>,
    counter: BlockUnitCounter,
    revealed: usize,
    last_tick: f64,
    first_buffered: Option<f64>,
    last_target: Option<f64>,
    arrival_rate: f64,
    carry: f64,
    pub timer_fps: Option<f64>,
    pub smooth: bool,
    pub hide_thinking: bool,
    pub fps: f64,
}
impl StreamingRevealController {
    pub fn new(smooth: bool, fps: f64, hide_thinking: bool) -> Self {
        Self {
            smooth,
            fps,
            hide_thinking,
            arrival_rate: 90.,
            ..Self::default()
        }
    }
    pub fn begin(&mut self, message: Value, now: f64) -> Value {
        self.stop();
        self.target = Some(message);
        if self.total() > 0 {
            self.first_buffered = Some(now);
            self.last_target = Some(now);
        }
        self.apply(now)
    }
    fn total(&mut self) -> usize {
        self.target.as_ref().map_or(0, |t| {
            count_visible_units(t, self.hide_thinking, &mut self.counter)
        })
    }
    pub fn set_target(&mut self, message: Value, now: f64) -> Value {
        let previous = self.total();
        self.target = Some(message);
        let appended = self.total().saturating_sub(previous);
        if appended > 0 {
            self.first_buffered.get_or_insert(now);
            if let Some(last) = self.last_target
                && now > last
            {
                self.arrival_rate =
                    update_arrival_rate(self.arrival_rate, appended as f64, now - last);
            }
            self.last_target = Some(now);
        }
        self.apply(now)
    }
    pub fn is_pacing_head(&self, message: &Value) -> bool {
        self.target.is_some() && self.smooth && !has_tool(message)
    }
    pub fn resync_visibility(&mut self, now: f64) -> Value {
        self.revealed = self.revealed.min(self.total());
        self.apply(now)
    }
    pub fn stop(&mut self) {
        self.target = None;
        self.timer_fps = None;
        self.revealed = 0;
        self.last_tick = 0.;
        self.counter.reset();
        self.first_buffered = None;
        self.last_target = None;
        self.arrival_rate = 90.;
        self.carry = 0.;
    }
    fn apply(&mut self, now: f64) -> Value {
        let total = self.total();
        let immediate = !self.smooth || self.target.as_ref().is_some_and(has_tool);
        if immediate {
            self.revealed = total;
            self.carry = 0.;
            self.timer_fps = None;
        } else {
            self.revealed = self.revealed.min(total);
            self.sync(total, now);
        }
        self.display()
    }
    fn display(&mut self) -> Value {
        self.target.as_ref().map_or(Value::Null, |t| {
            build_display_message(t, self.revealed, self.hide_thinking, &mut self.counter)
        })
    }
    fn sync(&mut self, total: usize, now: f64) {
        if self.revealed >= total {
            self.carry = 0.;
            self.timer_fps = None;
        } else {
            let fps = smooth_fps(self.fps);
            if self.timer_fps != Some(fps) {
                self.timer_fps = Some(fps);
                self.last_tick = now;
            }
        }
    }
    pub fn tick(&mut self, now: f64) -> Option<Value> {
        self.target.as_ref()?;
        let total = self.total();
        if self.revealed >= total {
            self.carry = 0.;
            self.timer_fps = None;
            return None;
        }
        let dt = now - self.last_tick;
        self.last_tick = now;
        if self
            .first_buffered
            .is_some_and(|t| now - t < INITIAL_BUFFER_MS)
        {
            return None;
        }
        self.carry += next_step((total - self.revealed) as f64, dt, self.arrival_rate);
        let step = self.carry.floor() as usize;
        if step == 0 {
            return None;
        }
        self.carry -= step as f64;
        self.revealed = total.min(self.revealed + step);
        let result = self.display();
        self.sync(total, now);
        Some(result)
    }
}
fn has_tool(message: &Value) -> bool {
    message
        .get("content")
        .and_then(Value::as_array)
        .is_some_and(|blocks| {
            blocks
                .iter()
                .any(|b| b.get("type").and_then(Value::as_str) == Some("toolCall"))
        })
}
