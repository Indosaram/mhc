use std::{cell::Cell, rc::Rc};
use maho_interactive::widget_clock::{ClockedWidget, WidgetFrameClock};
use maho_tui::tui::Component;

struct VirtualClock(Cell<u64>);
impl WidgetFrameClock for VirtualClock { fn now_ms(&self) -> u64 { self.0.get() } }
struct Widget { frames: Rc<Cell<u64>>, disposed: Rc<Cell<bool>> }
impl Widget {
    fn tick(&mut self) -> bool {
        if self.frames.get() == 14 { return false; }
        self.frames.set(self.frames.get() + 1); true
    }
}
impl Component for Widget {
    fn render(&mut self, _: usize) -> Vec<String> { vec![self.frames.get().to_string()] }
    fn dispose(&mut self) { self.disposed.set(true); }
}

#[test]
fn virtual_frame_clock_repaints_settles_and_disposes_without_sleep() {
    let clock = Rc::new(VirtualClock(Cell::new(0)));
    let frames = Rc::new(Cell::new(0));
    let disposed = Rc::new(Cell::new(false));
    let repaints = Rc::new(Cell::new(0));
    let repaint = repaints.clone();
    let mut widget = ClockedWidget::new(Widget { frames: frames.clone(), disposed: disposed.clone() }, Widget::tick, clock.clone(), Rc::new(move || repaint.set(repaint.get() + 1)));
    clock.0.set(64); assert!(!widget.advance()); assert_eq!(frames.get(), 0);
    clock.0.set(65); assert!(widget.advance()); assert_eq!(frames.get(), 1);
    clock.0.set(14 * 65); assert!(widget.advance()); assert_eq!(frames.get(), 14);
    clock.0.set(15 * 65); assert!(!widget.advance());
    clock.0.set(10000); assert!(!widget.advance()); assert_eq!(repaints.get(), 2);
    widget.dispose(); assert!(disposed.get()); assert!(!widget.advance());
}

#[tokio::test(start_paused = true)]
async fn timer_delivers_frame_events_and_disposal_cancels_subscription() {
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let frames = Rc::new(Cell::new(0));
    let mut widget = ClockedWidget::new(Widget { frames: frames.clone(), disposed: Rc::new(Cell::new(false)) }, Widget::tick,
        Rc::new(maho_interactive::widget_clock::MonotonicWidgetClock::default()), Rc::new(|| {})).with_frame_events(sender);
    assert!(matches!(receiver.recv().await, Some(maho_interactive::interactive_extension_ui::UiRequest::WidgetFrame)));
    assert!(widget.advance()); assert_eq!(frames.get(), 1);
    widget.dispose();
    assert!(receiver.recv().await.is_none());
}
