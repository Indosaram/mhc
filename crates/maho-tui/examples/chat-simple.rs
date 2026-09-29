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

    {
        let screen = Rc::clone(&screen);
        let is_responding = Rc::clone(&is_responding);
        let pending_response_due_ms = Rc::clone(&pending_response_due_ms);
        let pending_loader = Rc::clone(&pending_loader);
        let input_component_for_submit = Rc::clone(&input_component);
        input.borrow_mut().on_submit = Some(Box::new(move |value: &str| {
            if *is_responding.borrow() {
                return;
            }
            let trimmed = value.trim();

            if trimmed == "/delete" {
                let mut screen = screen.borrow_mut();
                let len = screen.base.children().len();
                if len > 3 {
                    let target = screen.base.children()[len - 2].clone();
                    screen.base.remove_child(&target);
                }
                screen.base.request_render(true, now_ms());
                return;
            }

            if trimmed == "/clear" {
                let mut screen = screen.borrow_mut();
                let children: Vec<_> = screen.base.children().to_vec();
                for child in children.iter().skip(2).take(children.len().saturating_sub(3)) {
                    screen.base.remove_child(child);
                }
                screen.base.request_render(true, now_ms());
                return;
            }

            if trimmed.is_empty() {
                return;
            }

            *is_responding.borrow_mut() = true;
            let mut screen_mut = screen.borrow_mut();
            let user_message: Rc<RefCell<dyn Component>> = Rc::new(RefCell::new(Text::new(value)));
            screen_mut.base.add_child(Rc::clone(&user_message));

            let now = now_ms();
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
            drop(screen_mut);

            *pending_response_due_ms.borrow_mut() = Some(now + 1000);
            let _ = &input_component_for_submit;
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

        if let Some(due) = *pending_response_due_ms.borrow()
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
