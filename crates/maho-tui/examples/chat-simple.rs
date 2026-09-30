//! Port of senpi `packages/tui/test/chat-simple.ts`: a simple chat interface demo driven by
//! `TuiMainScreen`.
//!
//! senpi's original uses `Editor` (with `CombinedAutocompleteProvider`) and `Markdown` for
//! message rendering; both are todo 8/9 owned and are 1-line stub placeholders in this lane
//! (see `crates/maho-tui/parity.d/7.md` for the N/A note). This port substitutes `Input`
//! (todo 7's single-line input, already ported) for `Editor` without autocomplete, and `Text`
//! for `Markdown` since this crate has no markdown renderer yet; everything else - slash
//! commands, the simulated bot response, and the child-splice bookkeeping - matches senpi's
//! `chat-simple.ts` line for line.

use std::cell::RefCell;
use std::rc::Rc;

use maho_tui::components::input::{Input, InputOptions};
use maho_tui::components::loader::Loader;
use maho_tui::components::text::Text;
use maho_tui::terminal::{ProcessTerminal, Terminal};
use maho_tui::tui::Component;
use maho_tui::tui_main_screen::TuiMainScreen;

const RESPONSES: [&str; 8] = [
    "That's interesting! Tell me more.",
    "I see what you mean.",
    "Fascinating perspective!",
    "Could you elaborate on that?",
    "That makes sense to me.",
    "I hadn't thought of it that way.",
    "Great point!",
    "Thanks for sharing that.",
];

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn main() {
    let screen = Rc::new(RefCell::new(TuiMainScreen::new()));

    let welcome: Rc<RefCell<dyn Component>> = Rc::new(RefCell::new(Text::new(
        "Welcome to Simple Chat!\n\nType your messages below. Type '/' for commands. Press Ctrl+C to exit.",
    )));
    screen.borrow_mut().base.add_child(Rc::clone(&welcome));

    let input: Rc<RefCell<Input>> = Rc::new(RefCell::new(Input::new(InputOptions::default())));
    let input_component: Rc<RefCell<dyn Component>> = input.clone() as Rc<RefCell<dyn Component>>;
    screen.borrow_mut().base.add_child(Rc::clone(&input_component));
    screen.borrow_mut().base.set_focus(Some(Rc::clone(&input_component)));

    let is_responding = Rc::new(RefCell::new(false));
    let pending_response_due_ms: Rc<RefCell<Option<u64>>> = Rc::new(RefCell::new(None));
    let pending_loader: Rc<RefCell<Option<Rc<RefCell<Loader>>>>> = Rc::new(RefCell::new(None));

    // `Input::handle_input` invokes `on_submit` while the main loop holds `screen.borrow_mut()`
    // (senpi's `editor.onSubmit` runs inside the same synchronous dispatch, where mutating
    // `tui.children` is free). A Rust closure cannot take a second borrow of `screen`, so the
    // handler only queues the submitted value and the main loop applies it once the dispatch
    // borrow has been released - the same queue-and-drain idiom as `pending_input` below.
    let pending_submits: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    {
        let pending_submits = Rc::clone(&pending_submits);
        input.borrow_mut().on_submit = Some(Box::new(move |value: &str| {
            pending_submits.borrow_mut().push(value.to_string());
        }));
    }

    let support = maho_tui::tui_main_screen::MouseTrackingSupport { is_tty: false, is_termux: false, is_windows_without_wt: false };
    let mut terminal = ProcessTerminal::default();

    // `ProcessTerminal::pump` invokes the input callback while it still holds `&mut self`, so
    // the callback cannot also take `&mut dyn Terminal` to drive `handle_mouse_input`/
    // `handle_terminal_input` (both need terminal column/row queries and mouse-tracking
    // writes). The callback only queues raw chunks; the main loop drains the queue and
    // dispatches with the real terminal once `pump` has returned and released its borrow.
    let pending_input: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    {
        let pending_input = Rc::clone(&pending_input);
        terminal.start(
            Box::new(move |data| {
                pending_input.borrow_mut().push(data.to_string());
            }),
            Box::new(|| {}),
        );
    }

    loop {
        let now = now_ms();

        let chunks: Vec<String> = pending_input.borrow_mut().drain(..).collect();
        for data in &chunks {
            let mut screen_mut = screen.borrow_mut();
            let handled_as_mouse = screen_mut.handle_mouse_input(data, now as i64, &mut terminal);
            if !handled_as_mouse {
                screen_mut.base.handle_terminal_input(data, false);
            }
        }

        let submits: Vec<String> = pending_submits.borrow_mut().drain(..).collect();
        if !submits.is_empty() {
            let mut screen_mut = screen.borrow_mut();
            for value in submits {
                apply_submit(&mut screen_mut, &value, now, &is_responding, &pending_response_due_ms, &pending_loader);
            }
        }

        let response_due = *pending_response_due_ms.borrow();
        if let Some(due) = response_due
            && now >= due
        {
            *pending_response_due_ms.borrow_mut() = None;
            let mut screen_mut = screen.borrow_mut();
            if let Some(loader) = pending_loader.borrow_mut().take() {
                let loader_component: Rc<RefCell<dyn Component>> = loader as Rc<RefCell<dyn Component>>;
                screen_mut.base.remove_child(&loader_component);
            }
            let response = RESPONSES[(now as usize / 7) % RESPONSES.len()];
            let bot_message: Rc<RefCell<dyn Component>> = Rc::new(RefCell::new(Text::new(response)));
            screen_mut.base.add_child(Rc::clone(&bot_message));
            *is_responding.borrow_mut() = false;
            screen_mut.base.request_render(true, now);
        }

        if screen.borrow().base.render_due(now) {
            let mut screen_mut = screen.borrow_mut();
            screen_mut.base.do_render(&mut terminal);
            screen_mut.note_render(&mut terminal);
        }

        if let Err(error) = terminal.pump(50) {
            eprintln!("stdin error: {error}");
            break;
        }
        if screen.borrow().base.state.borrow().stopped {
            break;
        }
    }

    let _ = &support;
    terminal.drain_input(500, 50);
    if let Err(error) = terminal.stop() {
        eprintln!("terminal restore failed: {error}");
    }
}

/// Body of senpi's `editor.onSubmit`, applied by the main loop after the input dispatch borrow
/// has been released (see `pending_submits` above).
fn apply_submit(
    screen_mut: &mut TuiMainScreen,
    value: &str,
    now: u64,
    is_responding: &Rc<RefCell<bool>>,
    pending_response_due_ms: &Rc<RefCell<Option<u64>>>,
    pending_loader: &Rc<RefCell<Option<Rc<RefCell<Loader>>>>>,
) {
    if *is_responding.borrow() {
        return;
    }
    let trimmed = value.trim();

    if trimmed == "/delete" {
        let len = screen_mut.base.children().len();
        if len > 3 {
            let target = screen_mut.base.children()[len - 2].clone();
            screen_mut.base.remove_child(&target);
        }
        screen_mut.base.request_render(true, now);
        return;
    }

    if trimmed == "/clear" {
        let children: Vec<_> = screen_mut.base.children().to_vec();
        for child in children.iter().skip(2).take(children.len().saturating_sub(3)) {
            screen_mut.base.remove_child(child);
        }
        screen_mut.base.request_render(true, now);
        return;
    }

    if trimmed.is_empty() {
        return;
    }

    *is_responding.borrow_mut() = true;
    let user_message: Rc<RefCell<dyn Component>> = Rc::new(RefCell::new(Text::new(value)));
    screen_mut.base.add_child(Rc::clone(&user_message));

    let loader = Rc::new(RefCell::new(Loader::new(
        Rc::new(|s: &str| format!("\x1b[36m{s}\x1b[0m")),
        Rc::new(|s: &str| format!("\x1b[2m{s}\x1b[0m")),
        "Thinking...",
        None,
        now,
    )));
    loader.borrow_mut().start(now);
    let loader_component: Rc<RefCell<dyn Component>> = loader.clone() as Rc<RefCell<dyn Component>>;
    screen_mut.base.add_child(Rc::clone(&loader_component));
    *pending_loader.borrow_mut() = Some(Rc::clone(&loader));
    screen_mut.base.request_render(true, now);

    *pending_response_due_ms.borrow_mut() = Some(now + 1000);
}
