//! Port of senpi `packages/coding-agent/src/modes/interactive/tips/tip-line.ts`.

use std::cell::RefCell;
use std::rc::Rc;

use maho_tui::components::spacer::Spacer;
use maho_tui::components::text::Text;
use maho_tui::tui::Container;

pub fn append_tip_line(container: &mut Container, tip_line: &str) -> Rc<RefCell<Text>> {
    container.add_child(Rc::new(RefCell::new(Spacer::new(1))));
    let tip = Rc::new(RefCell::new(Text::with_padding(tip_line, 1, 0)));
    container.add_child(Rc::clone(&tip) as Rc<RefCell<dyn maho_tui::tui::Component>>);
    tip
}
