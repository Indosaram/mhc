//! Port of src/tmux-utils/layout.test.ts

mod common;

use std::sync::{Arc, Mutex};

use common::strings;
use pretty_assertions::assert_eq;
use tmux_core::{LayoutDeps, TmuxLayout, apply_layout};

fn recording_deps() -> (LayoutDeps, Arc<Mutex<Vec<Vec<String>>>>) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&calls);
    let deps = LayoutDeps {
        spawn_command: Some(Arc::new(move |args: &[String]| {
            sink.lock().unwrap().push(args.to_vec());
            0
        })),
    };
    (deps, calls)
}

#[test]
fn applies_main_vertical_with_main_pane_width_option() {
    let (deps, calls) = recording_deps();
    apply_layout("tmux", TmuxLayout::MainVertical, 60, &deps);
    assert_eq!(
        *calls.lock().unwrap(),
        vec![
            strings(&["tmux", "select-layout", "main-vertical"]),
            strings(&["tmux", "set-window-option", "main-pane-width", "60%"]),
        ]
    );
}

#[test]
fn applies_main_horizontal_with_main_pane_height_option() {
    let (deps, calls) = recording_deps();
    apply_layout("tmux", TmuxLayout::MainHorizontal, 55, &deps);
    assert_eq!(
        *calls.lock().unwrap(),
        vec![
            strings(&["tmux", "select-layout", "main-horizontal"]),
            strings(&["tmux", "set-window-option", "main-pane-height", "55%"]),
        ]
    );
}

#[test]
fn does_not_set_main_pane_option_for_non_main_layouts() {
    let (deps, calls) = recording_deps();
    apply_layout("tmux", TmuxLayout::Tiled, 50, &deps);
    assert_eq!(
        *calls.lock().unwrap(),
        vec![strings(&["tmux", "select-layout", "tiled"])]
    );
}
