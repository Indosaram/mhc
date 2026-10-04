use std::{rc::Rc, sync::Arc, time::Duration};
use maho_tui::tui::Component;
use crate::components::todo_strike::TODO_STRIKE_FRAME_INTERVAL_MS;

pub trait WidgetFrameClock {
    fn now_ms(&self) -> u64;
}

pub struct MonotonicWidgetClock(tokio::time::Instant);
impl Default for MonotonicWidgetClock {
    fn default() -> Self { Self(tokio::time::Instant::now()) }
}
impl WidgetFrameClock for MonotonicWidgetClock {
    fn now_ms(&self) -> u64 { u64::try_from(self.0.elapsed().as_millis()).unwrap_or(u64::MAX) }
}

pub struct ClockedWidget<C: Component> {
    component: C,
    tick: fn(&mut C) -> bool,
    clock: Rc<dyn WidgetFrameClock>,
    next_frame_ms: Option<u64>,
    repaint: Rc<dyn Fn()>,
    timer: Option<tokio::task::JoinHandle<()>>,
    disposed: bool,
}

impl<C: Component> ClockedWidget<C> {
    pub fn new(component: C, tick: fn(&mut C) -> bool, clock: Rc<dyn WidgetFrameClock>, repaint: Rc<dyn Fn()>) -> Self {
        let next_frame_ms = Some(clock.now_ms().saturating_add(TODO_STRIKE_FRAME_INTERVAL_MS));
        Self { component, tick, clock, next_frame_ms, repaint, timer: None, disposed: false }
    }

    pub fn with_frame_events(mut self, sender: tokio::sync::mpsc::UnboundedSender<crate::interactive_extension_ui::UiRequest>) -> Self {
        self.timer = Some(tokio::spawn(async move {
            let start = tokio::time::Instant::now() + Duration::from_millis(TODO_STRIKE_FRAME_INTERVAL_MS);
            let mut interval = tokio::time::interval_at(start, Duration::from_millis(TODO_STRIKE_FRAME_INTERVAL_MS));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                if sender.send(crate::interactive_extension_ui::UiRequest::WidgetFrame).is_err() { break; }
            }
        }));
        self
    }

    pub fn advance(&mut self) -> bool {
        let now = self.clock.now_ms();
        let mut changed = false;
        while let Some(next) = self.next_frame_ms {
            if now < next { break; }
            if !(self.tick)(&mut self.component) {
                self.next_frame_ms = None;
                if let Some(timer) = self.timer.take() { timer.abort(); }
                break;
            }
            changed = true;
            self.next_frame_ms = Some(next.saturating_add(TODO_STRIKE_FRAME_INTERVAL_MS));
        }
        if changed { self.component.invalidate(); (self.repaint)(); }
        changed
    }
}

impl<C: Component> Component for ClockedWidget<C> {
    fn render(&mut self, width: usize) -> Vec<String> { self.advance(); self.component.render(width) }
    fn invalidate(&mut self) { self.component.invalidate(); }
    fn dispose(&mut self) {
        if self.disposed { return; }
        self.disposed = true;
        self.next_frame_ms = None;
        if let Some(timer) = self.timer.take() { timer.abort(); }
        self.component.dispose();
    }
}

impl<C: Component> Drop for ClockedWidget<C> {
    fn drop(&mut self) { self.dispose(); }
}

pub fn widget_content<C: Component + 'static>(
    factory: Arc<dyn Fn(&maho_ext_api::Theme) -> C + Send + Sync>,
    tick: fn(&mut C) -> bool,
    sender: tokio::sync::mpsc::UnboundedSender<crate::interactive_extension_ui::UiRequest>,
) -> maho_ext_api::WidgetContent {
    maho_ext_api::WidgetContent::Component(Arc::new(move |theme| {
        let repaint_sender = sender.clone();
        Box::new(ClockedWidget::new(factory(theme), tick, Rc::new(MonotonicWidgetClock::default()),
            Rc::new(move || { drop(repaint_sender.send(crate::interactive_extension_ui::UiRequest::WidgetFrame)); }))
            .with_frame_events(sender.clone()))
    }))
}
