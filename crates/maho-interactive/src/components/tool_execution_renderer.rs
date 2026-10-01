//! Port of `components/tool-execution-renderer.ts`.
use std::cell::RefCell;
use std::rc::Rc;

use maho_tools::definition::{RenderShell, ToolResult};
use maho_tui::components::box_::Box as TuiBox;
use maho_tui::components::text::Text;
use maho_tui::tui::{Component, Container};
use serde_json::Value;

use super::tool_execution_fallback::{
    create_tool_call_fallback, create_tool_result_fallback, format_tool_execution_fallback,
};
use super::tool_execution_types::ToolExecutionRenderState;
use super::tool_renderer_boundary::ToolRendererBoundary;
use crate::theme::{Theme, ThemeBg, ThemeColor};
use crate::tool_progress::{format_tool_progress_line, read_tool_progress};
use crate::tools::renderers::{ToolRenderContext, ToolRenderers, create_all_tool_renderers};

pub struct ToolExecutionIdentity {
    pub tool_name: String,
    pub tool_call_id: String,
    pub cwd: String,
}

pub struct ToolExecutionRenderer {
    identity: ToolExecutionIdentity,
    built_in: Option<Rc<RefCell<dyn ToolRenderers>>>,
    custom: Option<Rc<RefCell<dyn ToolRenderers>>>,
    /// `renderShell` of the built-in tool definition found by name, which is where the shell
    /// actually lives: `maho_tools`'s `edit` definition declares `RenderShell::Own`. The renderer
    /// registry only supplies `renderCall`/`renderResult`, so the shell has to be read from the
    /// definition.
    built_in_shell: RenderShell,
    content_box: TuiBox,
    content_text: Text,
    self_render_container: Container,
    state: ToolExecutionRenderState,
    theme: Theme,
    on_invalidate: Rc<dyn Fn()>,
    call_component: Option<Rc<RefCell<dyn Component>>>,
    result_component: Option<Rc<RefCell<dyn Component>>>,
}

impl ToolExecutionRenderer {
    pub fn new(
        identity: ToolExecutionIdentity,
        state: ToolExecutionRenderState,
        custom: Option<Rc<RefCell<dyn ToolRenderers>>>,
        theme: Theme,
        on_invalidate: Rc<dyn Fn()>,
    ) -> Self {
        let built_in = create_all_tool_renderers().get(identity.tool_name.as_str()).cloned();
        let built_in_shell = maho_tools::index::create_all_tool_definitions(
            std::path::Path::new(&identity.cwd),
            maho_tools::index::ToolsOptions::default(),
        )
        .get(&identity.tool_name)
        .and_then(|definition| definition.render_shell)
        .unwrap_or(RenderShell::Default);
        Self {
            identity,
            built_in,
            custom,
            built_in_shell,
            content_box: TuiBox::with_padding(1, 1),
            content_text: Text::with_padding(String::new(), 1, 1),
            self_render_container: Container::new(),
            state,
            theme,
            on_invalidate,
            call_component: None,
            result_component: None,
        }
    }

    pub fn has_renderer_definition(&self) -> bool {
        self.built_in.is_some() || self.custom.is_some()
    }

    pub fn has_result_renderer(&self) -> bool {
        // senpi's `hasResultRenderer` asks the renderer the card actually chose: the definition's
        // own, else the built-in's.
        match (&self.custom, &self.built_in) {
            (Some(custom), _) => custom.borrow().has_result_renderer(),
            (None, Some(built_in)) => built_in.borrow().has_result_renderer(),
            (None, None) => false,
        }
    }

    pub fn render_shell(&self) -> RenderShell {
        if let Some(custom) = &self.custom {
            let shell = custom.borrow().render_shell();
            if shell != RenderShell::Default {
                return shell;
            }
        }
        self.built_in_shell
    }

    fn get_call_renderer(&self) -> Option<Rc<RefCell<dyn ToolRenderers>>> {
        self.custom.clone().or_else(|| self.built_in.clone())
    }

    fn context(&self, _last_component: Option<&Rc<RefCell<dyn Component>>>) -> ToolRenderContext<'_> {
        ToolRenderContext {
            args: &self.state.args,
            tool_call_id: &self.identity.tool_call_id,
            cwd: &self.identity.cwd,
            execution_started: self.state.execution_started,
            args_complete: self.state.args_complete,
            is_partial: self.state.is_partial,
            expanded: self.state.expanded,
            show_images: self.state.show_images,
            is_error: self.state.is_error,
            has_result: self.state.result.is_some(),
            spinner_frame: self.state.spinner_frame,
            now_ms: 0.,
            invalidate: Rc::clone(&self.on_invalidate),
        }
    }

    pub fn update(&mut self, state: ToolExecutionRenderState) {
        self.state = state;
        let background: Rc<dyn Fn(&str) -> String> = if self.state.is_partial {
            let theme = self.theme.clone();
            Rc::new(move |text: &str| theme.bg(ThemeBg::ToolPendingBg, text))
        } else if self.state.is_error {
            let theme = self.theme.clone();
            Rc::new(move |text: &str| theme.bg(ThemeBg::ToolErrorBg, text))
        } else {
            let theme = self.theme.clone();
            Rc::new(move |text: &str| theme.bg(ThemeBg::ToolSuccessBg, text))
        };

        let progress = if self.state.is_partial {
            self.state.result.as_ref().and_then(|result| result.details.as_ref()).and_then(read_tool_progress)
        } else {
            None
        };

        if !self.has_renderer_definition() {
            self.content_text.set_custom_bg_fn(Some(background));
            let result = self.state.result.as_ref().map(|result| result.as_tool_result());
            let mut text = format_tool_execution_fallback(
                &self.identity.tool_name,
                &self.state.args,
                result.as_ref(),
                self.state.show_images,
                &self.theme,
            );
            if let Some(progress) = &progress {
                text += &format!("\n{}", format_tool_progress_line(progress, 0., self.state.spinner_frame));
            }
            self.content_text.set_text(text);
            return;
        }

        let use_self = self.render_shell() == RenderShell::Own;
        if use_self {
            self.self_render_container.clear();
        } else {
            self.content_box.set_bg_fn(Some(background));
            self.content_box.detach_all();
        }
        // senpi's edit renderer rebuilds its call component inside `renderResult`, because the call
        // body and its background show the preview the result settles. A renderer that asks for
        // this gets its call child built after the result has run, then committed first so the
        // child order still matches senpi's.
        let rebuild_call_after_result = self
            .get_call_renderer()
            .is_some_and(|renderer| renderer.borrow().rebuild_call_after_result());
        let mut call_child = if rebuild_call_after_result { None } else { self.render_call() };
        let result_child = if self.state.result.is_some() { self.render_result() } else { None };
        if rebuild_call_after_result {
            call_child = self.render_call();
        }
        if let Some(child) = call_child {
            self.add_child(use_self, child);
        }
        if let Some(child) = result_child {
            self.add_child(use_self, child);
        }
        if let Some(progress) = &progress {
            let line = format_tool_progress_line(progress, 0., self.state.spinner_frame);
            let child = Rc::new(RefCell::new(Text::with_padding(line, 0, 0))) as Rc<RefCell<dyn Component>>;
            self.add_child(use_self, child);
        }
    }

    fn add_child(&mut self, use_self: bool, component: Rc<RefCell<dyn Component>>) {
        if use_self {
            self.self_render_container.add_child(component);
        } else {
            self.content_box.add_child(component);
        }
    }

    fn render_call(&mut self) -> Option<Rc<RefCell<dyn Component>>> {
        let fallback = create_tool_call_fallback(&self.identity.tool_name, &self.theme);
        let Some(renderer) = self.get_call_renderer() else {
            return Some(fallback);
        };
        let component = {
            let last = self.call_component.clone();
            let context = self.context(last.as_ref());
            renderer.borrow_mut().render_call(&self.theme, &context)
        };
        match component {
            Some(component) => {
                self.call_component = Some(Rc::clone(&component));
                let boundary = Rc::new(RefCell::new(ToolRendererBoundary::new(
                    component,
                    Some(fallback),
                    Box::new(|| {}),
                ))) as Rc<RefCell<dyn Component>>;
                Some(boundary)
            }
            None => {
                self.call_component = None;
                Some(fallback)
            }
        }
    }

    fn render_result(&mut self) -> Option<Rc<RefCell<dyn Component>>> {
        let result = self.state.result.clone()?;
        let tool_result = result.as_tool_result();
        let fallback = create_tool_result_fallback(Some(&tool_result), self.state.show_images, &self.theme);
        let Some(renderer) = self.get_call_renderer() else {
            return fallback;
        };
        let options = crate::tools::renderers::ToolRenderResultOptions {
            expanded: self.state.expanded,
            is_partial: self.state.is_partial,
        };
        let component = {
            let last = self.result_component.clone();
            let context = self.context(last.as_ref());
            renderer.borrow_mut().render_result(&tool_result, options, &self.theme, &context)
        };
        match component {
            Some(component) => {
                self.result_component = Some(Rc::clone(&component));
                let boundary = Rc::new(RefCell::new(ToolRendererBoundary::new(
                    component,
                    fallback,
                    Box::new(|| {}),
                ))) as Rc<RefCell<dyn Component>>;
                Some(boundary)
            }
            None => {
                self.result_component = None;
                fallback
            }
        }
    }
}

impl Component for ToolExecutionRenderer {
    fn render(&mut self, width: usize) -> Vec<String> {
        if !self.has_renderer_definition() {
            return self.content_text.render(width);
        }
        if self.render_shell() == RenderShell::Own {
            self.self_render_container.render(width)
        } else {
            self.content_box.render(width)
        }
    }
    fn invalidate(&mut self) {
        self.content_box.invalidate();
        self.self_render_container.invalidate();
        self.content_text.invalidate();
    }
    fn dispose(&mut self) {
        self.content_box.dispose();
        self.self_render_container.dispose();
    }
}

pub fn is_error_result(result: &ToolResult) -> bool {
    result.details.as_ref().and_then(|details| details.get("isError")).and_then(Value::as_bool).unwrap_or(false)
}

pub fn title_color(theme: &Theme) -> String {
    theme.fg(ThemeColor::ToolTitle, "")
}
