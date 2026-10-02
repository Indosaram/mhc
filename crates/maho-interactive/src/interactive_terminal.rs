use std::{cell::RefCell, rc::Rc};
use maho_tui::{terminal::{Terminal, TerminalError}, tui::Component};
use crate::{interactive_mode::InteractiveMode, tui_renderer::InteractiveTui};

pub struct InteractiveTerminal {
    pub mode: Rc<RefCell<InteractiveMode>>,
    pub renderer: Rc<RefCell<InteractiveTui>>,
    input: Rc<RefCell<std::collections::VecDeque<String>>>,
    resized: Rc<std::cell::Cell<bool>>,
    mouse_enabled: bool,
    started: bool,
}

impl InteractiveTerminal {
    pub fn new(mode: InteractiveMode, mut renderer: InteractiveTui) -> Self {
        let mode = Rc::new(RefCell::new(mode));
        let root: Rc<RefCell<dyn Component>> = mode.clone();
        match &mut renderer {
            InteractiveTui::Regular(tui) => tui.base.add_child(root.clone()),
            InteractiveTui::Fullscreen(tui) => {
                let empty=||Rc::new(RefCell::new(maho_tui::components::text::Text::with_padding("",0,0))) as Rc<RefCell<dyn Component>>;
                let viewport=crate::chat_viewport::create_chat_viewport(crate::chat_viewport::ChatViewportOptions {
                    document:Rc::new(RefCell::new(ModeSection {mode:mode.clone(),document:true})),
                    editor:Rc::new(RefCell::new(ModeSection {mode:mode.clone(),document:false})),
                    pending_messages:empty(),status:empty(),footer:empty(),hook_status:None,widgets_above:None,widgets_below:None,
                    scrollbar:None,scrollbar_track_style:None,scrollbar_thumb_style:None,
                });
                tui.set_layout_root(Some(viewport.root));
            }
        }
        renderer.base_mut().set_focus(Some(root));
        let renderer = Rc::new(RefCell::new(renderer));
        mode.borrow_mut().set_mounted_renderer(renderer.clone());
        Self { mode, renderer, input:Rc::new(RefCell::new(Default::default())), resized:Rc::new(std::cell::Cell::new(false)), mouse_enabled:false, started:false }
    }

    pub fn start(&mut self, terminal: &mut dyn Terminal, mouse_enabled: bool, multiplexer: bool) {
        if self.started { return; }
        self.mouse_enabled = mouse_enabled;
        self.renderer.borrow_mut().before_terminal_start(terminal, mouse_enabled, multiplexer);
        let input = self.input.clone();
        let resized = self.resized.clone();
        terminal.start(Box::new(move |data| input.borrow_mut().push_back(data.into())), Box::new(move || resized.set(true)));
        self.renderer.borrow().base().state.borrow_mut().stopped = false;
        self.started = true;
        self.render(terminal);
    }

    pub fn take_input(&mut self, terminal: &mut dyn Terminal, now_ms: i64) -> Option<String> {
        loop {
            let input = self.input.borrow_mut().pop_front()?;
            if self.renderer.borrow().base().has_overlay() {
                self.renderer.borrow_mut().base_mut().handle_terminal_input(&input, false);
                continue;
            }
            if !self.renderer.borrow_mut().handle_mouse_input(&input, now_ms, terminal) { return Some(input); }
        }
    }

    pub fn render(&mut self, terminal: &mut dyn Terminal) {
        if !self.started { return; }
        self.mode.borrow().set_terminal_dimensions(usize::from(terminal.columns()),usize::from(terminal.rows()));
        self.mode.borrow_mut().drain_events();
        self.mode.borrow_mut().tick_now();
        if self.resized.replace(false) { self.renderer.borrow_mut().base_mut().invalidate(); }
        let (cursor, shrink, progress, title) = {
            let mode = self.mode.borrow();
            let (cursor, shrink, progress) = mode.terminal_settings();
            (cursor, shrink, progress && !mode.agent_idle, mode.terminal_title.clone())
        };
        self.renderer.borrow_mut().base_mut().set_show_hardware_cursor(cursor);
        self.renderer.borrow_mut().base_mut().set_clear_on_shrink(shrink);
        terminal.set_progress(progress);
        if let Some(title) = title { terminal.set_title(&title); }
        self.renderer.borrow_mut().do_render(terminal);
    }

    pub fn stop(&mut self, terminal: &mut dyn Terminal, preserve_screen: bool) -> Result<(), TerminalError> {
        if !self.started { return Ok(()); }
        self.renderer.borrow_mut().before_terminal_stop(terminal, self.mouse_enabled);
        terminal.set_progress(false);
        terminal.drain_input(100, 10);
        let result = terminal.stop();
        self.renderer.borrow_mut().after_terminal_stop(terminal, preserve_screen);
        self.renderer.borrow().base().state.borrow_mut().stopped = true;
        self.renderer.borrow_mut().base_mut().reset_for_stop();
        self.started = false;
        result
    }
}

struct ModeSection { mode:Rc<RefCell<InteractiveMode>>,document:bool }
impl Component for ModeSection {
    fn render(&mut self,width:usize)->Vec<String> {
        if self.document {self.mode.borrow_mut().render_document(width)} else {self.mode.borrow_mut().render_dock(width)}
    }
    fn invalidate(&mut self) {self.mode.borrow_mut().invalidate();}
}
