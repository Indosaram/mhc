use maho_ext_builtin_loose::files::collect_files;
use serde_json::json;

#[test]
fn executed_files_coalesce_operations_and_use_result_timestamp() {
    let branch = vec![
        json!({"type":"message","message":{"role":"assistant","content":[
            {"type":"toolCall","id":"r","name":"read","arguments":{"path":"a"}},
            {"type":"toolCall","id":"w","name":"write","arguments":{"path":"a"}},
            {"type":"toolCall","id":"e","name":"edit","arguments":{"path":"b"}},
            {"type":"toolCall","id":"unused","name":"read","arguments":{"path":"unexecuted"}}
        ]}}),
        json!({"type":"message","message":{"role":"toolResult","toolCallId":"r","timestamp":1}}),
        json!({"type":"message","message":{"role":"toolResult","toolCallId":"w","timestamp":3}}),
        json!({"type":"message","message":{"role":"toolResult","toolCallId":"e","timestamp":2}}),
    ];
    let files = collect_files(&branch);
    assert_eq!(files.len(), 2);
    assert_eq!(files[0].path, "a");
    assert_eq!(files[0].last_timestamp, 3);
    assert_eq!(files[0].operations.iter().map(String::as_str).collect::<Vec<_>>(), vec!["read", "write"]);
    assert_eq!(files[1].path, "b");
}

#[test]
fn native_picker_pages_opens_and_closes_without_finishing_on_selection(){
    use maho_ext_builtin_loose::files::FilePicker;
    use maho_ext_api::{Theme,CustomUiDone};
    use maho_tui::{components::select_list::SelectItem,tui::Component};
    use std::{rc::Rc,cell::Cell,sync::{Arc,Mutex}};
    let opened=Arc::new(Mutex::new(Vec::new()));let captured=opened.clone();
    let renders=Rc::new(Cell::new(0));let repaint=renders.clone();
    let closed=Rc::new(Cell::new(false));let finished=closed.clone();
    let done:CustomUiDone=Rc::new(move|_|finished.set(true));
    let items=(0..30).map(|index|SelectItem{value:index.to_string(),label:format!("file{index}"),description:None}).collect();
    let mut panel=FilePicker::new("Files",items,Theme::default(),Rc::new(move||repaint.set(repaint.get()+1)),done,Arc::new(move|index|captured.lock().expect("open").push(index)));
    panel.handle_input("\x1b[C");panel.handle_input("\r");
    assert_eq!(*opened.lock().expect("open"),[15]);assert!(!closed.get());
    panel.handle_input("\x1b[D");panel.handle_input("\r");
    assert_eq!(*opened.lock().expect("open"),[15,0]);
    panel.handle_input("\x1b");assert!(closed.get());assert_eq!(renders.get(),5);
}
