use std::{sync::{Arc, Mutex}, time::Duration};
use tokio::{task::JoinHandle, time::Instant};
use maho_core::project_trust::AppMode;
pub struct StartupLoadingIndicatorOptions {
    pub writer: Arc<dyn Fn(&str) + Send + Sync>, pub is_tty: bool, pub label: String,
    pub grace_ms: u64, pub interval_ms: u64, pub frames: Vec<String>,
}
impl StartupLoadingIndicatorOptions {
    pub fn new(writer: impl Fn(&str) + Send + Sync + 'static, is_tty: bool) -> Self {
        Self { writer: Arc::new(writer), is_tty, label: "Loading".to_owned(), grace_ms: 120, interval_ms: 80, frames: "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏".chars().map(|frame| frame.to_string()).collect() }
    }
}
struct State {
    options: StartupLoadingIndicatorOptions, phase: Option<String>, frame: usize,
    drawn: bool, grace_elapsed: bool, started: bool, paused: bool, stopped: bool,
    next: Option<Instant>,
}
impl State {
    fn draw(&mut self, hide_cursor: bool) {
        let frame = self.options.frames.get(self.frame).map_or("", String::as_str);
        let phase = self.phase.as_deref().filter(|phase| !phase.is_empty()).map_or(String::new(), |phase| format!(" {phase}"));
        (self.options.writer)(&format!("{}\r\x1b[2K\x1b[2m{frame} {}…{phase}\x1b[0m", if hide_cursor { "\x1b[?25l" } else { "" }, self.options.label));
        self.drawn = true;
    }
    fn erase(&mut self) { if self.drawn { (self.options.writer)("\r\x1b[2K\x1b[?25h"); self.drawn = false; } }
    fn advance(&mut self, now: Instant) {
        if self.paused || self.stopped { return; }
        while let Some(next) = self.next.filter(|next| *next <= now) {
            if self.grace_elapsed {
                self.frame = (self.frame + 1) % self.options.frames.len(); self.draw(false);
            } else { self.grace_elapsed = true; if !self.drawn { self.draw(true); } }
            self.next = Some(next + Duration::from_millis(self.options.interval_ms.max(1)));
        }
    }
}
pub struct StartupLoadingIndicator { state: Arc<Mutex<State>>, timer: Option<JoinHandle<()>> }
impl StartupLoadingIndicator {
    pub fn new(mut options: StartupLoadingIndicatorOptions) -> Self {
        if options.frames.is_empty() { options.frames = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏".chars().map(|frame| frame.to_string()).collect(); }
        Self { state: Arc::new(Mutex::new(State { options, phase: None, frame: 0, drawn: false, grace_elapsed: false, started: false, paused: false, stopped: false, next: None })), timer: None }
    }
    pub fn running(&self) -> bool { let state = self.state.lock().expect("indicator state"); state.started && !state.stopped }
    fn arm(&mut self) {
        let state = self.state.clone();
        self.timer = Some(tokio::spawn(async move {
            loop {
                let next = state.lock().expect("indicator state").next;
                let Some(next) = next else { return; };
                tokio::time::sleep_until(next).await;
                state.lock().expect("indicator state").advance(Instant::now());
            }
        }));
    }
    pub fn advance(&self, now: Instant) { self.state.lock().expect("indicator state").advance(now); }
    pub fn start(&mut self) {
        { let mut state = self.state.lock().expect("indicator state");
            if !state.options.is_tty || state.started || state.stopped { return; }
            state.started = true; state.draw(true); state.next = Some(Instant::now() + Duration::from_millis(state.options.grace_ms));
        } self.arm();
    }
    pub fn set_phase(&self, phase: Option<String>) { let mut state = self.state.lock().expect("indicator state"); state.phase = phase; if state.drawn && !state.paused && !state.stopped { state.draw(false); } }
    pub fn pause(&mut self) {
        let mut state = self.state.lock().expect("indicator state");
        if !state.started || state.stopped || state.paused { return; }
        state.paused = true; state.next = None; state.erase();
        if let Some(timer) = self.timer.take() { timer.abort(); }
    }
    pub fn resume(&mut self) {
        { let mut state = self.state.lock().expect("indicator state");
            if !state.started || state.stopped || !state.paused { return; }
            state.paused = false; state.draw(true);
            let delay = if state.grace_elapsed { state.options.interval_ms.max(1) } else { state.options.grace_ms };
            state.next = Some(Instant::now() + Duration::from_millis(delay));
        } self.arm();
    }
    pub fn stop(&mut self) {
        let mut state = self.state.lock().expect("indicator state");
        if state.stopped { return; } state.stopped = true; state.next = None; state.erase();
        if let Some(timer) = self.timer.take() { timer.abort(); }
    }
    pub async fn during_prompt<T>(&mut self, prompt: impl std::future::Future<Output = T>) -> T {
        self.pause(); let result = prompt.await; self.resume(); result
    }
    /// senpi `pauseIndicatorDuringPrompts`: another writer must not land inside the unterminated
    /// frame, so the frame is erased first and redrawn once the write finished.
    pub fn during_surface_write<T>(&mut self, write: impl FnOnce() -> T) -> T {
        self.pause(); let result = write(); self.resume(); result
    }
}
impl Drop for StartupLoadingIndicator { fn drop(&mut self) { self.stop(); } }
pub fn should_show_startup_loading_indicator(mode: AppMode, stdout_is_tty: bool, help_requested: bool) -> bool { mode == AppMode::Interactive && stdout_is_tty && !help_requested }
