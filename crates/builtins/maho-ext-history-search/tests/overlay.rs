use maho_ext_history_search::{overlay::HistorySearchOverlay,types::HistoryEntry};
use maho_interactive::theme::{Theme,ColorMode};
use maho_tui::tui::{Component,Focusable};
use std::{rc::Rc,cell::{Cell,RefCell}};
#[test]
fn synthetic_filter_and_selection(){
    let entries=["ship release","build project","write tests"].iter().map(|text|HistoryEntry{text:(*text).into(),session_id:"session".into(),cwd:"/repo".into(),session_file:"session.jsonl".into(),timestamp:0}).collect();
    let renders=Rc::new(Cell::new(0));let render_count=Rc::clone(&renders);let selected=Rc::new(RefCell::new(None));let result=Rc::clone(&selected);
    let mut overlay=HistorySearchOverlay::new(entries,Theme::builtin("dark",ColorMode::Truecolor).expect("theme"),Rc::new(move||render_count.set(render_count.get()+1)),Rc::new(move|entry|*result.borrow_mut()=entry));
    overlay.set_focused(true);overlay.handle_input("b");assert_eq!(overlay.search_value(),"b");assert_eq!(overlay.filtered_entries()[0].text,"build project");assert_eq!(renders.get(),1);
    assert!(!overlay.render(80).is_empty());overlay.handle_input("\r");assert_eq!(selected.borrow().as_ref().expect("selection").text,"build project");
}

#[test]
fn cancel_reports_no_selection_after_filtering(){
    let entries=vec![HistoryEntry{text:"build project".into(),session_id:"session".into(),cwd:"/repo".into(),session_file:"session.jsonl".into(),timestamp:0}];
    let outcome=Rc::new(RefCell::new(None));let captured=outcome.clone();
    let mut overlay=HistorySearchOverlay::new(entries,Theme::builtin("dark",ColorMode::Truecolor).expect("theme"),Rc::new(||{}),Rc::new(move|entry|*captured.borrow_mut()=Some(entry)));
    overlay.handle_input("b");overlay.handle_input("\x1b");
    assert_eq!(*outcome.borrow(),Some(None));
}
