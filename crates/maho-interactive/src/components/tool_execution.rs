//! Port of `components/tool-execution.ts`.
//!
//! senpi schedules the spinner and todo-strike animations with Node timers; this port keeps the
//! same frame arithmetic but advances frames on explicit host ticks, so rendering stays
//! deterministic and testable.
use std::cell::RefCell;
use std::rc::Rc;

use maho_tools::definition::ToolContent;
use maho_tui::components::spacer::Spacer;
use maho_tui::tui::Component;
use serde_json::Value;

use super::render_signature::create_bounded_render_signature;
use super::todo_strike::{TODO_STRIKE_FRAME_INTERVAL_MS, TODO_STRIKE_TOTAL_FRAMES, has_completed_todo_tasks};
use super::tool_execution_images::{ToolExecutionImageOptions, ToolExecutionImages};
use super::tool_execution_renderer::ToolExecutionRenderer;
use super::tool_execution_types::{ToolExecutionRenderState, ToolExecutionResult};
use crate::theme::{Theme, ThemeColor};
use crate::tool_progress::read_tool_progress;
use crate::tools::renderers::ToolRenderers;

const FALLBACK_PREVIEW_LINES: usize = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolExecutionPresentation {
    Classic,
    Grok,
}

/// State the grok chrome pushes into its tool row. The row component itself lives with the grok
/// chrome; `components/tool-execution.ts` only names it, so the seam is a trait here.
#[derive(Clone, Debug)]
pub struct GrokToolRowState {
    pub tool_name: String,
    pub is_partial: bool,
    pub result_is_error: Option<bool>,
    pub has_result: bool,
}

pub trait GrokToolRowComponent: Component {
    fn update_state(&mut self, state: &GrokToolRowState);
}

#[derive(Clone, Debug, Default)]
pub struct ToolExecutionOptions {
    pub show_images: Option<bool>,
    pub image_width_cells: Option<u32>,
}

fn collapse_fallback_result(
    result: Option<&ToolExecutionResult>,
    show_images: bool,
    expanded: bool,
    theme: &Theme,
) -> Option<ToolExecutionResult> {
    let result = result?;
    if expanded {
        return Some(result.clone());
    }
    let output = crate::tools::render_utils::get_text_output(Some(&result.as_tool_result()), show_images);
    if output.is_empty() {
        return Some(result.clone());
    }
    let lines: Vec<&str> = output.split('\n').collect();
    if lines.len() <= FALLBACK_PREVIEW_LINES {
        return Some(result.clone());
    }
    let remaining = lines.len() - FALLBACK_PREVIEW_LINES;
    let hint = crate::components::keybinding_hints::key_hint("app.tools.expand", "to expand", theme);
    let text = format!(
        "{}{}{}{}",
        lines[..FALLBACK_PREVIEW_LINES].join("\n"),
        theme.fg(ThemeColor::Muted, &format!("\n... ({remaining} more lines,")),
        hint,
        theme.fg(ThemeColor::Muted, ")")
    );
    Some(ToolExecutionResult { content: vec![ToolContent::text(text)], details: result.details.clone(), is_error: result.is_error })
}

pub struct ToolExecutionComponent {
    identity: ToolExecutionRendererIdentity,
    renderer: Option<ToolExecutionRenderer>,
    images: Option<ToolExecutionImages>,
    grok_row: Option<Rc<RefCell<dyn GrokToolRowComponent>>>,
    presentation: ToolExecutionPresentation,
    args: Value,
    expanded: bool,
    show_images: bool,
    image_width_cells: u32,
    is_partial: bool,
    execution_started: bool,
    args_complete: bool,
    spinner_frame: Option<i64>,
    todo_strike_frames_left: Option<i64>,
    result: Option<ToolExecutionResult>,
    cached_lines: Option<Vec<String>>,
    cached_signature: Option<String>,
    cached_width: Option<usize>,
    last_display_signature: Option<String>,
    theme: Theme,
    on_change: Option<Rc<dyn Fn()>>,
    spinner_tick_ms: Option<u64>,
    custom: Option<Rc<RefCell<dyn ToolRenderers>>>,
}

struct ToolExecutionRendererIdentity {
    tool_name: String,
    tool_call_id: String,
    cwd: String,
}

impl ToolExecutionComponent {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        tool_name: &str,
        tool_call_id: &str,
        args: Value,
        options: ToolExecutionOptions,
        custom: Option<Rc<RefCell<dyn ToolRenderers>>>,
        cwd: &str,
        presentation: ToolExecutionPresentation,
        grok_row: Option<Rc<RefCell<dyn GrokToolRowComponent>>>,
        theme: Theme,
    ) -> Self {
        let identity = ToolExecutionRendererIdentity {
            tool_name: tool_name.to_owned(),
            tool_call_id: tool_call_id.to_owned(),
            cwd: cwd.to_owned(),
        };
        let mut component = Self {
            identity,
            renderer: None,
            images: None,
            grok_row: None,
            presentation,
            args,
            expanded: false,
            show_images: options.show_images.unwrap_or(true),
            image_width_cells: options.image_width_cells.unwrap_or(60),
            is_partial: true,
            execution_started: false,
            args_complete: false,
            spinner_frame: None,
            todo_strike_frames_left: None,
            result: None,
            cached_lines: None,
            cached_signature: None,
            cached_width: None,
            last_display_signature: None,
            theme,
            on_change: None,
            spinner_tick_ms: None,
            custom: custom.clone(),
        };
        let initial = component.create_render_state();
        if presentation == ToolExecutionPresentation::Grok {
            component.grok_row = grok_row;
        } else {
            component.renderer = Some(ToolExecutionRenderer::new(
                super::tool_execution_renderer::ToolExecutionIdentity {
                    tool_name: component.identity.tool_name.clone(),
                    tool_call_id: component.identity.tool_call_id.clone(),
                    cwd: component.identity.cwd.clone(),
                },
                initial,
                custom,
                component.theme.clone(),
                Rc::new(|| {}),
            ));
            component.images = Some(ToolExecutionImages::new(component.theme.clone()));
        }
        component.update_spinner_animation();
        component.update_display();
        component
    }

    pub fn set_on_change(&mut self, callback: Option<Rc<dyn Fn()>>) {
        self.on_change = callback;
    }

    /// senpi keeps a card out of an exploration group when its definition replaced the built-in
    /// renderers. A definition that passes the built-in pair unchanged is still built-in.
    pub fn uses_built_in_renderers(&self) -> bool {
        self.custom.as_ref().is_none_or(|custom| custom.borrow().is_built_in())
    }

    fn request_render(&self) {
        if let Some(on_change) = &self.on_change {
            on_change();
        }
    }

    pub fn update_args(&mut self, args: Value) {
        self.args = args;
        self.last_display_signature = None;
        self.update_spinner_animation();
        self.update_display();
    }

    pub fn presentation_snapshot(&self) -> (ToolExecutionRenderState, String, ToolExecutionPresentation) {
        (self.create_render_state(), self.identity.tool_name.clone(), self.presentation)
    }

    pub fn identity(&self) -> (String, String, String) {
        (self.identity.tool_name.clone(), self.identity.tool_call_id.clone(), self.identity.cwd.clone())
    }

    pub fn mark_execution_started(&mut self) {
        self.execution_started = true;
        self.update_spinner_animation();
        self.update_display();
        self.request_render();
    }

    pub fn set_args_complete(&mut self) {
        self.args_complete = true;
        self.update_spinner_animation();
        self.update_display();
        self.request_render();
    }

    pub fn update_result(&mut self, result: ToolExecutionResult, is_partial: bool) {
        self.result = Some(result.clone());
        self.is_partial = is_partial;
        if !is_partial {
            self.args_complete = true;
        }
        self.last_display_signature = None;
        self.update_spinner_animation();
        self.update_todo_strike_animation();
        self.update_display();
        if let Some(images) = &mut self.images {
            images.update_result(&result.as_tool_result());
        }
        self.invalidate_render_cache();
    }

    pub fn stop_animation(&mut self) {
        self.spinner_frame = None;
        self.todo_strike_frames_left = None;
    }

    pub fn set_expanded(&mut self, expanded: bool) {
        self.expanded = expanded;
        self.update_display();
    }

    pub fn is_expanded(&self) -> bool {
        self.expanded
    }

    pub fn set_show_images(&mut self, show: bool) {
        self.show_images = show;
        self.update_display();
    }

    pub fn set_image_width_cells(&mut self, width: u32) {
        self.image_width_cells = width.max(1);
        self.update_display();
    }

    pub fn tick(&mut self, now_ms: u64) -> bool {
        let mut changed = false;
        if self.spinner_frame.is_some() {
            let due = self.spinner_tick_ms.is_none_or(|last| now_ms.saturating_sub(last) >= 80);
            if due {
                self.spinner_frame = Some((self.spinner_frame.unwrap_or(-1) + 1) % 10);
                self.spinner_tick_ms = Some(now_ms);
                self.invalidate_render_cache();
                self.update_display();
                changed = true;
            }
        }
        if let Some(frames_left) = self.todo_strike_frames_left {
            let due = self
                .spinner_tick_ms
                .is_none_or(|last| now_ms.saturating_sub(last) >= TODO_STRIKE_FRAME_INTERVAL_MS);
            if due {
                let next = frames_left - 1;
                if next < 0 {
                    self.todo_strike_frames_left = None;
                    if self.spinner_frame.is_none() {
                        self.spinner_frame = None;
                    }
                } else {
                    self.todo_strike_frames_left = Some(next);
                    self.spinner_frame = Some(TODO_STRIKE_TOTAL_FRAMES - next);
                }
                self.spinner_tick_ms = Some(now_ms);
                self.invalidate_render_cache();
                self.update_display();
                changed = true;
            }
        }
        changed
    }

    fn update_display(&mut self) {
        let display_signature = self.create_render_signature();
        if self.last_display_signature.as_ref() == Some(&display_signature) {
            return;
        }
        self.last_display_signature = Some(display_signature);
        self.invalidate_render_cache();
        let state = self.create_render_state();
        if self.grok_row.is_some() {
            let is_error = self.is_error();
            let row_state = GrokToolRowState {
                tool_name: self.identity.tool_name.clone(),
                is_partial: state.is_partial,
                result_is_error: self.result.as_ref().map(|_| is_error),
                has_result: state.result.is_some(),
            };
            if let Some(grok_row) = &mut self.grok_row {
                grok_row.borrow_mut().update_state(&row_state);
            }
            return;
        }
        let (Some(renderer), Some(images)) = (&mut self.renderer, &mut self.images) else {
            return;
        };
        let has_result_renderer = renderer.has_result_renderer();
        let render_state = if renderer.has_renderer_definition() && !has_result_renderer {
            let collapsed = collapse_fallback_result(state.result.as_ref(), state.show_images, state.expanded, &self.theme);
            ToolExecutionRenderState { result: collapsed, ..state }
        } else {
            state
        };
        renderer.update(render_state);
        images.update_options(ToolExecutionImageOptions {
            show_images: self.show_images,
            max_width_cells: self.image_width_cells,
            show_renderer_fallback: has_result_renderer,
        });
    }

    fn is_error(&self) -> bool {
        self.result.as_ref().is_some_and(|result| result.is_error)
    }

    fn create_render_state(&self) -> ToolExecutionRenderState {
        let result = self.result.as_ref().map(|result| ToolExecutionResult {
            content: result
                .content
                .iter()
                .filter(|part| !maho_tools::model_only_text::is_model_only_text(part))
                .cloned()
                .collect(),
            details: result.details.clone(),
            is_error: result.is_error,
        });
        ToolExecutionRenderState {
            args: self.args.clone(),
            execution_started: self.execution_started,
            args_complete: self.args_complete,
            is_partial: self.is_partial,
            expanded: self.expanded,
            show_images: self.show_images,
            spinner_frame: self.spinner_frame,
            result,
            is_error: self.is_error(),
        }
    }

    fn create_render_signature(&self) -> String {
        let state = self.create_render_state();
        create_bounded_render_signature(&serde_json::json!({
            "args": state.args,
            "executionStarted": state.execution_started,
            "argsComplete": state.args_complete,
            "isPartial": state.is_partial,
            "expanded": state.expanded,
            "showImages": state.show_images,
            "spinnerFrame": state.spinner_frame,
            "result": state.result.map(|result| serde_json::json!({
                "content": result.content,
                "details": result.details,
            })),
            "imageWidthCells": self.image_width_cells,
            "toolCallId": self.identity.tool_call_id,
            "toolName": self.identity.tool_name,
        }))
    }

    fn update_spinner_animation(&mut self) {
        let is_streaming_args = !self.args_complete
            && matches!(self.identity.tool_name.as_str(), "edit" | "write" | "apply_patch");
        let is_partial_task = self.is_partial && self.identity.tool_name == "task" && self.result.is_some();
        let is_partial_progress = self.is_partial
            && self
                .result
                .as_ref()
                .and_then(|result| result.details.as_ref())
                .is_some_and(|details| read_tool_progress(details).is_some());
        if is_streaming_args || is_partial_task || is_partial_progress {
            self.spinner_frame.get_or_insert(0);
        } else {
            self.spinner_frame = None;
        }
    }

    fn update_todo_strike_animation(&mut self) {
        let should_animate = self.identity.tool_name == "todo"
            && self.execution_started
            && !self.is_partial
            && self.result.is_some()
            && !self.is_error()
            && self
                .result
                .as_ref()
                .and_then(|result| result.details.as_ref())
                .is_some_and(has_completed_todo_tasks);
        if !should_animate {
            self.todo_strike_frames_left = None;
            return;
        }
        if self.todo_strike_frames_left.is_none() {
            self.spinner_frame = Some(0);
            self.todo_strike_frames_left = Some(TODO_STRIKE_TOTAL_FRAMES);
        }
    }

    fn invalidate_render_cache(&mut self) {
        self.cached_lines = None;
        self.cached_signature = None;
        self.cached_width = None;
    }
}

impl Component for ToolExecutionComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        if self.presentation == ToolExecutionPresentation::Grok {
            let mut lines: Vec<String> = Vec::new();
            lines.extend(Spacer::new(1).render(width));
            if let Some(grok_row) = &mut self.grok_row {
                lines.extend(grok_row.borrow_mut().render(width));
            }
            return lines;
        }

        let signature = self.create_render_signature();
        if let Some(cached) = &self.cached_lines
            && self.cached_width == Some(width)
            && self.cached_signature.as_ref() == Some(&signature)
        {
            return cached.clone();
        }

        let mut lines: Vec<String> = Vec::new();
        let (Some(renderer), Some(images)) = (&mut self.renderer, &mut self.images) else {
            return lines;
        };
        if renderer.has_renderer_definition() && renderer.render_shell() == maho_tools::definition::RenderShell::Own {
            let content_lines = renderer.render(width);
            let image_lines = images.render(width);
            if content_lines.is_empty() && image_lines.is_empty() {
                return Vec::new();
            }
            if content_lines.is_empty() {
                lines.extend(image_lines);
            } else {
                lines.extend(Spacer::new(1).render(width));
                lines.extend(content_lines);
                lines.extend(image_lines);
            }
        } else {
            lines.extend(Spacer::new(1).render(width));
            lines.extend(renderer.render(width));
            lines.extend(images.render(width));
        }

        self.cached_width = Some(width);
        self.cached_signature = Some(signature);
        self.cached_lines = Some(lines.clone());
        lines
    }

    fn invalidate(&mut self) {
        self.invalidate_render_cache();
        if let Some(renderer) = &mut self.renderer {
            renderer.invalidate();
        }
        if let Some(images) = &mut self.images {
            images.invalidate();
        }
        self.last_display_signature = None;
        self.update_display();
    }

    fn dispose(&mut self) {
        self.stop_animation();
        if let Some(renderer) = &mut self.renderer {
            renderer.dispose();
        }
        if let Some(images) = &mut self.images {
            images.dispose();
        }
    }
}

pub fn result_is_error(result: &ToolExecutionResult) -> bool {
    result.is_error
}

pub fn format_elapsed(elapsed_ms: f64) -> String {
    crate::working_status::format_working_elapsed_seconds(elapsed_ms / 1000.)
}
