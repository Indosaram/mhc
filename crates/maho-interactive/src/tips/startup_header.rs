//! Port of senpi `packages/coding-agent/src/modes/interactive/tips/startup-header.ts`.

use std::cell::RefCell;
use std::rc::Rc;

use maho_tui::components::spacer::Spacer;
use maho_tui::components::text::Text;
use maho_tui::tui::{Component, Container};

use super::tip_line::append_tip_line;

pub fn append_startup_header(
    container: &mut Container,
    header: Rc<RefCell<dyn Component>>,
    tip_line: Option<&str>,
) -> Option<Rc<RefCell<Text>>> {
    container.add_child(Rc::new(RefCell::new(Spacer::new(1))));
    container.add_child(header);

    let tip_component = tip_line.map(|tip_line| append_tip_line(container, tip_line));

    container.add_child(Rc::new(RefCell::new(Spacer::new(1))));
    tip_component
}
