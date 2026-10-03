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

#[test]
fn retained_terminal_rows_resize_and_clamp_existing_scroll(){
    let rows=Rc::new(Cell::new(12));let dimensions=rows.clone();
    let content=(0..30).map(|index|format!("paragraph {index}")).collect::<Vec<_>>().join("\n\n");
    let mut panel=HelpPanel::new(&content,Theme::builtin("dark",ColorMode::Truecolor).expect("theme"),Rc::new(move||dimensions.get()),Rc::new(||{}),Rc::new(||{}));
    assert_eq!(panel.render(60).len(),8);
    panel.handle_input("\x1b[6~");
    rows.set(6);assert_eq!(panel.render(60).len(),2);
    rows.set(100);let expanded=panel.render(60);
    let mut fresh=HelpPanel::new(&content,Theme::builtin("dark",ColorMode::Truecolor).expect("theme"),Rc::new(||100),Rc::new(||{}),Rc::new(||{}));
    assert_eq!(expanded,fresh.render(60));
}
