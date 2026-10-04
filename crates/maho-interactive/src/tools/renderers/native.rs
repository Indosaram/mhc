//! Stateful native extension cards. Each tool call owns separate call/result slots.
use std::{cell::RefCell, collections::HashMap, path::PathBuf, rc::Rc, sync::Arc};
use maho_ext_api::{AgentToolResult, ToolRendererSession, ToolRendererSlots};
use maho_tui::tui::Component;
use maho_tools::definition::ToolResult;
use serde_json::Value;
use super::{RenderedComponent, ToolRenderContext, ToolRenderResultOptions, ToolRenderers};
use crate::theme::Theme;

pub struct NativeToolRenderers<S> {
    renderers: Arc<maho_ext_api::ToolRenderers<S, Value>>,
    cards: HashMap<String, Rc<RefCell<ToolRendererSlots<S, Value>>>>,
}

impl<S: Default + 'static> NativeToolRenderers<S> {
    pub fn new(renderers: Arc<maho_ext_api::ToolRenderers<S, Value>>) -> Self {
        let render_call = renderers.render_call.clone().map(|renderer| Arc::new(move |args: &Value, theme: &maho_ext_api::Theme, context: &mut maho_ext_api::ToolRenderContext<S, Value>| -> Box<dyn Component> {
            Box::new(OwnedComponent(renderer(args, theme, context)))
        }) as maho_ext_api::ToolCallRenderer<S, Value>);
        let render_result = renderers.render_result.clone().map(|renderer| Arc::new(move |result: &AgentToolResult, options, theme: &maho_ext_api::Theme, context: &mut maho_ext_api::ToolRenderContext<S, Value>| -> Box<dyn Component> {
            Box::new(OwnedComponent(renderer(result, options, theme, context)))
        }) as maho_ext_api::ToolResultRenderer<S, Value>);
        Self { renderers: Arc::new(maho_ext_api::ToolRenderers { render_call, render_result }), cards: HashMap::new() }
    }

    fn slots(&mut self, source: &ToolRenderContext<'_>) -> Rc<RefCell<ToolRendererSlots<S, Value>>> {
        let slots = self.cards.entry(source.tool_call_id.to_owned()).or_insert_with(|| {
            Rc::new(RefCell::new(ToolRendererSession {
                renderers: self.renderers.clone(),
                context: maho_ext_api::ToolRenderContext {
                    args: source.args.clone(), tool_call_id: source.tool_call_id.to_owned(),
                    invalidate: source.invalidate.clone(), last_component: None, state: S::default(),
                    cwd: PathBuf::from(source.cwd), execution_started: source.execution_started,
                    args_complete: source.args_complete, is_partial: source.is_partial,
                    expanded: source.expanded, show_images: source.show_images, image_protocol: None,
                    is_error: source.is_error, has_result: Some(source.has_result),
                    spinner_frame: source.spinner_frame.and_then(|frame| usize::try_from(frame).ok()),
                },
            }.into_slots()))
        }).clone();
        {
            let mut slots = slots.borrow_mut();
            let context = &mut slots.session.context;
            context.args = source.args.clone();
            context.cwd = PathBuf::from(source.cwd);
            context.invalidate = source.invalidate.clone();
            context.execution_started = source.execution_started;
            context.args_complete = source.args_complete;
            context.is_partial = source.is_partial;
            context.expanded = source.expanded;
            context.show_images = source.show_images;
            context.is_error = source.is_error;
            context.has_result = Some(source.has_result);
            context.spinner_frame = source.spinner_frame.and_then(|frame| usize::try_from(frame).ok());
        }
        slots
    }
}

struct NativeCardHalf<S> {
    slots: Rc<RefCell<ToolRendererSlots<S, Value>>>,
    theme: maho_ext_api::Theme,
    result: Option<AgentToolResult>,
}

impl<S: 'static> Component for NativeCardHalf<S> {
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut slots = self.slots.borrow_mut();
        match &self.result {
            Some(result) => slots.render_result(result, &self.theme, width),
            None => slots.render_call(&self.theme, width),
        }.unwrap_or_default()
    }
}

struct OwnedComponent(Box<dyn Component>);
impl Component for OwnedComponent {
    fn render(&mut self, width: usize) -> Vec<String> { self.0.render(width) }
    fn invalidate(&mut self) { self.0.invalidate(); }
}
impl Drop for OwnedComponent {
    fn drop(&mut self) { self.0.dispose(); }
}

impl<S: Default + 'static> ToolRenderers for NativeToolRenderers<S> {
    fn has_result_renderer(&self) -> bool { self.renderers.render_result.is_some() }
    fn render_call(&mut self, theme: &Theme, context: &ToolRenderContext<'_>) -> Option<RenderedComponent> {
        self.renderers.render_call.as_ref()?;
        Some(Rc::new(RefCell::new(NativeCardHalf {
            slots: self.slots(context), theme: crate::interactive_extension_ui::extension_theme(theme), result: None,
        })))
    }
    fn render_result(&mut self, result: &ToolResult, options: ToolRenderResultOptions, theme: &Theme, context: &ToolRenderContext<'_>) -> Option<RenderedComponent> {
        self.renderers.render_result.as_ref()?;
        let slots = self.slots(context);
        slots.borrow_mut().session.context.expanded = options.expanded;
        slots.borrow_mut().session.context.is_partial = options.is_partial;
        Some(Rc::new(RefCell::new(NativeCardHalf {
            slots, theme: crate::interactive_extension_ui::extension_theme(theme),
            result: Some(super::registered::to_agent_result(result)),
        })))
    }
}
