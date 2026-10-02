use maho_ext_help::panel::{HelpPanel,HELP_OVERLAY_MARGIN};
use maho_interactive::theme::{Theme,ColorMode};
use maho_tui::tui::Component;
use std::{rc::Rc,cell::Cell};
#[test]
fn viewport_pages_and_closes(){
    let content=(0..20).map(|index|format!("paragraph {index}")).collect::<Vec<_>>().join("\n\n");
    let closed=Rc::new(Cell::new(0));let close=Rc::clone(&closed);let renders=Rc::new(Cell::new(0));let render=Rc::clone(&renders);
    let mut panel=HelpPanel::new(&content,Theme::builtin("dark",ColorMode::Truecolor).expect("theme"),Rc::new(||8),Rc::new(move||render.set(render.get()+1)),Rc::new(move||close.set(close.get()+1)));
    let first=panel.render(60);assert_eq!(first.len(),8-HELP_OVERLAY_MARGIN*2);panel.handle_input("\x1b[6~");assert_ne!(panel.render(60),first);assert_eq!(renders.get(),1);panel.handle_input("\x1b");assert_eq!(closed.get(),1);
}
