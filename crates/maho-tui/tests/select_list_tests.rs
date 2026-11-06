use maho_tui::components::select_list::{SelectItem, SelectList, SelectListLayoutOptions};
use maho_tui::tui::Component;

use crate::common::plain_select_list_theme;

fn item(value: &str) -> SelectItem {
    SelectItem {
        value: value.to_string(),
        label: value.to_string(),
        description: None,
    }
}

fn filtered_to_nothing() -> SelectList {
    let items = vec![item("alpha"), item("beta")];
    let mut list = SelectList::new(
        items,
        5,
        plain_select_list_theme(),
        SelectListLayoutOptions::default(),
    );
    list.set_filter("zzz");
    list
}

#[test]
fn up_on_empty_filter_keeps_list_usable() {
    let mut list = filtered_to_nothing();
    list.handle_input("\x1b[A");
    assert!(list.get_selected_item().is_none());
    list.set_filter("");
    assert_eq!(
        list.get_selected_item().map(|item| item.value.as_str()),
        Some("alpha")
    );
}

#[test]
fn down_on_empty_filter_keeps_list_usable() {
    let mut list = filtered_to_nothing();
    list.handle_input("\x1b[B");
    assert!(list.get_selected_item().is_none());
    list.set_filter("");
    assert_eq!(
        list.get_selected_item().map(|item| item.value.as_str()),
        Some("alpha")
    );
}
