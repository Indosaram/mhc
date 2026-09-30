//! Port of senpi `packages/tui/src/layout.ts`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::components::scroll_view::ScrollView;
use crate::image_stub::{crop_kitty_image_line, get_kitty_image_metadata, is_image_line};
use crate::layout_node::{
    LayoutNode, StackAlign, StackBasis, StackDirection, StackLayoutEntry, LayoutViewport,
};
use crate::tui::{composite_tui_line, Component, CURSOR_MARKER};
use crate::utils::{extract_ansi_code, get_active_background_ansi, get_grapheme_cell_range, slice_by_column, visible_width};

/// OSC 133 shell-integration zone markers (A/B/C) at the start of a line; senpi strips a run of
/// these before painting so they never occupy a terminal cell.
pub(crate) fn strip_osc133_zone_prefix(line: &str) -> &str {
    let mut rest = line;
    loop {
        let Some(tail) = rest.strip_prefix("\x1b]133;") else {
            return rest;
        };
        let Some(marker) = tail.as_bytes().first().copied() else {
            return rest;
        };
        if !matches!(marker, b'A' | b'B' | b'C') {
            return rest;
        }
        let after_marker = &tail[1..];
        if let Some(stripped) = after_marker.strip_prefix('\x07') {
            rest = stripped;
        } else if let Some(stripped) = after_marker.strip_prefix("\x1b\\") {
            rest = stripped;
        } else {
            return rest;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LayoutRect {
    pub x: i64,
    pub y: i64,
    pub width: usize,
    pub height: usize,
}

impl LayoutRect {
    fn right(&self) -> i64 {
        self.x + self.width as i64
    }

    fn bottom(&self) -> i64 {
        self.y + self.height as i64
    }
}

fn intersect(a: LayoutRect, b: LayoutRect) -> LayoutRect {
    let x = a.x.max(b.x);
    let y = a.y.max(b.y);
    let right = a.right().min(b.right());
    let bottom = a.bottom().min(b.bottom());
    LayoutRect {
        x,
        y,
        width: (right - x).max(0) as usize,
        height: (bottom - y).max(0) as usize,
    }
}

fn contains_point(rect: LayoutRect, x: i64, y: i64) -> bool {
    x >= rect.x && x < rect.right() && y >= rect.y && y < rect.bottom()
}

/// A laid-out component and its screen-space geometry, mirroring senpi's `LayoutBox`.
pub struct LayoutBox {
    pub component: Rc<RefCell<dyn Component>>,
    pub rect: LayoutRect,
    pub clip: LayoutRect,
    pub children: Vec<LayoutBox>,
    pub lines: Option<Vec<String>>,
    pub line_offset: usize,
    pub scroll_view: Option<Rc<RefCell<ScrollView>>>,
    pub scroll_content_lines: Option<Vec<String>>,
    pub layer: i64,
}

pub struct LayoutFrame {
    pub root: LayoutBox,
    pub width: usize,
    pub height: usize,
    pub lines: Vec<String>,
    pub primary_scroll_view: Option<Rc<RefCell<ScrollView>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollbarGeometry {
    pub column: i64,
    pub track_top: i64,
    pub track_height: usize,
    pub thumb_top: i64,
    pub thumb_height: usize,
    pub max_scroll_top: usize,
}

type RenderCache = HashMap<usize, Rc<RefCell<HashMap<usize, Rc<Vec<String>>>>>>;

struct LayoutContext {
    viewport: LayoutViewport,
    render_cache: RenderCache,
    primary_scroll_view: Option<Rc<RefCell<ScrollView>>>,
}

fn component_key(component: &Rc<RefCell<dyn Component>>) -> usize {
    Rc::as_ptr(component) as *const () as usize
}

fn render_cached(context: &mut LayoutContext, component: &Rc<RefCell<dyn Component>>, width: usize) -> Rc<Vec<String>> {
    let safe_width = width.max(1);
    let key = component_key(component);
    let widths = context
        .render_cache
        .entry(key)
        .or_insert_with(|| Rc::new(RefCell::new(HashMap::new())));
    if let Some(lines) = widths.borrow().get(&safe_width) {
        return Rc::clone(lines);
    }
    let lines = Rc::new(component.borrow_mut().render(safe_width));
    widths.borrow_mut().insert(safe_width, Rc::clone(&lines));
    lines
}

fn measure_height(context: &mut LayoutContext, component: &Rc<RefCell<dyn Component>>, width: usize) -> usize {
    render_cached(context, component, width).len()
}

fn measure_width(context: &mut LayoutContext, component: &Rc<RefCell<dyn Component>>, width: usize) -> usize {
    render_cached(context, component, width)
        .iter()
        .map(|line| visible_width(line))
        .max()
        .unwrap_or(0)
}

fn translate_box(root: &mut LayoutBox, delta_y: i64) {
    root.rect.y += delta_y;
    for child in &mut root.children {
        translate_box(child, delta_y);
    }
}

fn update_clips(root: &mut LayoutBox, parent_clip: LayoutRect) {
    root.clip = intersect(parent_clip, root.rect);
    let child_clip = root.clip;
    for child in &mut root.children {
        update_clips(child, child_clip);
    }
}

/// Allocate stack child sizes; a straight port of senpi's `allocateStackSizes` (see
/// `components/stack.rs`, todo 7).
fn allocate_stack_sizes(
    entries: &[StackLayoutEntry],
    intrinsic_sizes: &[usize],
    available: Option<usize>,
    gap: usize,
) -> Vec<usize> {
    crate::components::stack::allocate_stack_sizes(entries, intrinsic_sizes, available, gap)
}

fn visible_stack_entries(entries: &[StackLayoutEntry], viewport: LayoutViewport) -> Vec<StackLayoutEntry> {
    crate::components::stack::visible_stack_entries(entries, viewport)
}

#[allow(clippy::too_many_arguments)]
fn layout_component(
    context: &mut LayoutContext,
    component: &Rc<RefCell<dyn Component>>,
    x: i64,
    y: i64,
    width: usize,
    height: Option<usize>,
    clip: LayoutRect,
) -> LayoutBox {
    let safe_width = width.max(1);
    let node = component
        .borrow()
        .as_layout_component()
        .map(|c| c.layout_node());

    let Some(node) = node else {
        let lines = render_cached(context, component, safe_width);
        let allocated_height = height.unwrap_or(lines.len());
        let mut line_offset = 0;
        if lines.len() > allocated_height
            && allocated_height > 0
            && let Some(cursor_line) = lines.iter().position(|line| line.contains(CURSOR_MARKER))
            && cursor_line >= allocated_height
        {
            line_offset = cursor_line - allocated_height + 1;
        }
        let rect = LayoutRect {
            x,
            y,
            width: safe_width,
            height: allocated_height,
        };
        return LayoutBox {
            component: Rc::clone(component),
            rect,
            clip: intersect(clip, rect),
            children: Vec::new(),
            lines: Some((*lines).clone()),
            line_offset,
            scroll_view: None,
            scroll_content_lines: None,
            layer: 0,
        };
    };

    match node {
        LayoutNode::Scroll(scroll_node) => {
            let previous_scroll_top = scroll_node.state.borrow().scroll_top();
            let content_width = scroll_node.state.borrow().get_content_width(safe_width);
            let mut child_box = layout_component(
                context,
                &scroll_node.component,
                x,
                y - previous_scroll_top as i64,
                content_width,
                None,
                clip,
            );
            let content_height = child_box.rect.height;
            let viewport_height = height.unwrap_or(content_height);
            scroll_node
                .state
                .borrow_mut()
                .update_layout(content_height, viewport_height);
            let next_scroll_top = scroll_node.state.borrow().scroll_top();
            translate_box(&mut child_box, previous_scroll_top as i64 - next_scroll_top as i64);

            let scroll_view = Rc::clone(&scroll_node.state);
            if scroll_node.state.borrow().primary() || context.primary_scroll_view.is_none() {
                context.primary_scroll_view = Some(Rc::clone(&scroll_view));
            }
            let rect = LayoutRect {
                x,
                y,
                width: safe_width,
                height: viewport_height,
            };
            let child_clip = intersect(clip, rect);
            let scroll_content_lines = (*render_cached(context, &scroll_node.component, content_width)).clone();
            let mut boxed = LayoutBox {
                component: Rc::clone(component),
                rect,
                clip: child_clip,
                children: vec![child_box],
                lines: None,
                line_offset: 0,
                scroll_view: Some(scroll_view),
                scroll_content_lines: Some(scroll_content_lines),
                layer: 0,
            };
            update_clips(&mut boxed.children[0], child_clip);
            boxed
        }
        LayoutNode::Stack(stack_node) => {
            let entries = visible_stack_entries(&stack_node.entries, context.viewport);
            let gap_total = entries.len().saturating_sub(1) * stack_node.gap;
            match stack_node.direction {
                StackDirection::VStack => {
                    let intrinsic_heights: Vec<usize> = entries
                        .iter()
                        .map(|entry| match entry.basis {
                            Some(StackBasis::Fixed(size)) => size,
                            _ => measure_height(context, &entry.component, safe_width),
                        })
                        .collect();
                    let sizes = allocate_stack_sizes(&entries, &intrinsic_heights, height, stack_node.gap);
                    let natural_height: usize = sizes.iter().sum::<usize>() + gap_total;
                    let allocated_height = height.unwrap_or(natural_height);
                    let rect = LayoutRect {
                        x,
                        y,
                        width: safe_width,
                        height: allocated_height,
                    };
                    let mut boxed = LayoutBox {
                        component: Rc::clone(component),
                        rect,
                        clip: intersect(clip, rect),
                        children: Vec::new(),
                        lines: None,
                        line_offset: 0,
                        scroll_view: None,
                        scroll_content_lines: None,
                        layer: 0,
                    };
                    let mut child_y = y;
                    for (index, entry) in entries.iter().enumerate() {
                        let mut child = layout_component(
                            context,
                            &entry.component,
                            x,
                            child_y,
                            safe_width,
                            Some(sizes[index]),
                            boxed.clip,
                        );
                        child.layer = 0;
                        boxed.children.push(child);
                        child_y += sizes[index] as i64 + stack_node.gap as i64;
                    }
                    boxed
                }
                StackDirection::HStack => {
                    let intrinsic_widths: Vec<usize> = entries
                        .iter()
                        .map(|entry| match entry.basis {
                            Some(StackBasis::Fixed(size)) => size,
                            _ => measure_width(context, &entry.component, safe_width),
                        })
                        .collect();
                    let widths = allocate_stack_sizes(&entries, &intrinsic_widths, Some(safe_width), stack_node.gap);
                    let intrinsic_heights: Vec<usize> = entries
                        .iter()
                        .enumerate()
                        .map(|(index, entry)| measure_height(context, &entry.component, widths[index].max(1)))
                        .collect();
                    let allocated_height = height.unwrap_or_else(|| intrinsic_heights.iter().copied().max().unwrap_or(0));
                    let rect = LayoutRect {
                        x,
                        y,
                        width: safe_width,
                        height: allocated_height,
                    };
                    let mut boxed = LayoutBox {
                        component: Rc::clone(component),
                        rect,
                        clip: intersect(clip, rect),
                        children: Vec::new(),
                        lines: None,
                        line_offset: 0,
                        scroll_view: None,
                        scroll_content_lines: None,
                        layer: 0,
                    };
                    let mut child_x = x;
                    for (index, entry) in entries.iter().enumerate() {
                        let natural_child_height = intrinsic_heights[index];
                        let child_height = if stack_node.align == StackAlign::Stretch {
                            allocated_height
                        } else {
                            allocated_height.min(natural_child_height)
                        };
                        let mut child_y = y;
                        match stack_node.align {
                            StackAlign::Center => {
                                child_y += ((allocated_height - child_height) / 2) as i64;
                            }
                            StackAlign::End => {
                                child_y += (allocated_height - child_height) as i64;
                            }
                            _ => {}
                        }
                        let child_width = widths[index];
                        if child_width == 0 {
                            boxed.children.push(LayoutBox {
                                component: Rc::clone(&entry.component),
                                rect: LayoutRect {
                                    x: child_x,
                                    y: child_y,
                                    width: 0,
                                    height: child_height,
                                },
                                clip: LayoutRect {
                                    x: child_x,
                                    y: child_y,
                                    width: 0,
                                    height: 0,
                                },
                                children: Vec::new(),
                                lines: None,
                                line_offset: 0,
                                scroll_view: None,
                                scroll_content_lines: None,
                                layer: 0,
                            });
                        } else {
                            boxed.children.push(layout_component(
                                context,
                                &entry.component,
                                child_x,
                                child_y,
                                child_width,
                                Some(child_height),
                                boxed.clip,
                            ));
                        }
                        child_x += child_width as i64 + stack_node.gap as i64;
                    }
                    boxed
                }
            }
        }
    }
}

fn replace_scrollbar_cell(
    line: &str,
    column: i64,
    total_width: usize,
    replacement: &str,
    preserve_target_background: bool,
) -> String {
    if is_image_line(line) {
        return line.to_string();
    }
    let column = column.max(0) as usize;
    let range = get_grapheme_cell_range(line, column);
    let start = range.map(|r| r.start).unwrap_or(column);
    let end = range.map(|r| r.end).unwrap_or(column + 1);
    let before = slice_by_column(line, 0, start, true);
    let target = slice_by_column(line, start, end - start, true);
    let after = slice_by_column(line, end, total_width.saturating_sub(end), true);

    let mut target_prefix = String::new();
    let mut target_index = 0;
    while target_index < target.len() {
        match extract_ansi_code(&target, target_index) {
            Some(code) => {
                target_prefix.push_str(code);
                target_index += code.len();
            }
            None => break,
        }
    }
    let before_padding = " ".repeat(start.saturating_sub(visible_width(&before)));
    let cell_padding_before = " ".repeat(column.saturating_sub(start));
    let cell_padding_after = " ".repeat((end.saturating_sub(column)).saturating_sub(1));
    let target_style = format!(
        "\x1b[0m\x1b]8;;\x07{}",
        if preserve_target_background {
            get_active_background_ansi(&target_prefix)
        } else {
            String::new()
        }
    );
    format!("{before}{before_padding}{target_style}{cell_padding_before}{replacement}{cell_padding_after}{after}")
}

/// senpi's `getScrollbarGeometry`.
pub fn get_scrollbar_geometry(boxed: &LayoutBox, include_hidden_auto: bool) -> Option<ScrollbarGeometry> {
    let scroll_view = boxed.scroll_view.as_ref()?;
    if boxed.rect.width == 0 || boxed.rect.height == 0 {
        return None;
    }
    let content_height = boxed
        .children
        .first()
        .map(|c| c.rect.height)
        .or_else(|| boxed.scroll_content_lines.as_ref().map(Vec::len))
        .unwrap_or(0);
    let track_height = boxed.rect.height;
    let view = scroll_view.borrow();
    let can_reveal_hidden_auto = include_hidden_auto
        && view.scrollbar() == crate::components::scroll_view::ScrollViewScrollbar::Auto
        && content_height > track_height;
    if !view.is_scrollbar_visible() && !can_reveal_hidden_auto {
        return None;
    }

    let min_thumb_height = 2usize.min(track_height);
    let thumb_height = if content_height == 0 {
        track_height
    } else {
        min_thumb_height.max(
            ((track_height as f64 * track_height as f64) / content_height as f64)
                .round() as usize,
        )
        .min(track_height)
    };
    let max_scroll_top = content_height.saturating_sub(track_height);
    let max_thumb_top = track_height.saturating_sub(thumb_height);
    let thumb_offset = if max_scroll_top == 0 {
        0
    } else {
        ((view.scroll_top() as f64 / max_scroll_top as f64) * max_thumb_top as f64).round() as usize
    };
    let column = boxed.rect.x + boxed.rect.width as i64 - 1;
    if column < boxed.clip.x || column >= boxed.clip.x + boxed.clip.width as i64 {
        return None;
    }

    Some(ScrollbarGeometry {
        column,
        track_top: boxed.rect.y,
        track_height,
        thumb_top: boxed.rect.y + thumb_offset as i64,
        thumb_height,
        max_scroll_top,
    })
}

fn paint_scrollbar(boxed: &LayoutBox, screen: &mut [String], total_width: usize) {
    let Some(geometry) = get_scrollbar_geometry(boxed, false) else {
        return;
    };
    let Some(scroll_view) = boxed.scroll_view.as_ref() else {
        return;
    };
    let view = scroll_view.borrow();
    for offset in 0..geometry.track_height {
        let row = geometry.track_top + offset as i64;
        if row < boxed.clip.y || row >= boxed.clip.y + boxed.clip.height as i64 || row < 0 {
            continue;
        }
        let Some(row_index) = usize::try_from(row).ok().filter(|&r| r < screen.len()) else {
            continue;
        };
        let is_thumb = row >= geometry.thumb_top && row < geometry.thumb_top + geometry.thumb_height as i64;
        let replacement = if is_thumb {
            view.scrollbar_thumb_style(if view.is_scrollbar_active() { "\u{2588}" } else { "\u{2503}" })
        } else {
            view.scrollbar_track_style("\u{2502}")
        };
        screen[row_index] = replace_scrollbar_cell(
            &screen[row_index],
            geometry.column,
            total_width,
            &replacement,
            view.scrollbar() != crate::components::scroll_view::ScrollViewScrollbar::Always,
        );
    }
}

fn paint_box(boxed: &LayoutBox, screen: &mut Vec<String>, total_width: usize) {
    if let Some(lines) = &boxed.lines {
        let first_row = boxed.rect.y.max(boxed.clip.y).max(0);
        let last_row = boxed.rect.bottom().min(boxed.clip.bottom()).min(screen.len() as i64);
        let mut row = first_row;
        while row < last_row {
            let source_index = boxed.line_offset as i64 + row - boxed.rect.y;
            let Some(source_line) = (source_index >= 0)
                .then(|| lines.get(source_index as usize))
                .flatten()
            else {
                row += 1;
                continue;
            };
            let mut line = strip_osc133_zone_prefix(source_line).to_string();
            if let Some(rows) = get_kitty_image_metadata(&line) {
                let clip_bottom = (boxed.clip.bottom()).min(screen.len() as i64);
                let visible_rows = (rows as i64).min(clip_bottom - row).max(0) as usize;
                if visible_rows < rows {
                    line = crop_kitty_image_line(&line, 0, visible_rows);
                }
            }
            let row_index = row as usize;
            if boxed.rect.x == 0
                && boxed.rect.width >= total_width
                && (is_image_line(&line) || screen[row_index].is_empty())
            {
                screen[row_index] = line;
            } else {
                screen[row_index] = composite_tui_line(&screen[row_index], &line, boxed.rect.x.max(0) as usize, boxed.rect.width, total_width);
            }
            row += 1;
        }
    }
    for child in &boxed.children {
        paint_box(child, screen, total_width);
    }

    if let (Some(scroll_view), Some(scroll_content_lines)) = (&boxed.scroll_view, &boxed.scroll_content_lines) {
        let scroll_top = scroll_view.borrow().scroll_top();
        if scroll_top > 0 && boxed.rect.height > 0 {
            let mut image_row = scroll_top as i64 - 1;
            while image_row >= 0 {
                let image_line = scroll_content_lines
                    .get(image_row as usize)
                    .map(String::as_str)
                    .unwrap_or("");
                if let Some(rows) = get_kitty_image_metadata(image_line) {
                    let hidden_rows = scroll_top - image_row as usize;
                    if hidden_rows < rows {
                        let visible_rows = boxed.rect.height.min(rows - hidden_rows);
                        let cropped = crop_kitty_image_line(image_line, hidden_rows, visible_rows);
                        if boxed.rect.x == 0 && boxed.rect.width >= total_width {
                            let y = boxed.rect.y;
                            if y >= 0 && (y as usize) < screen.len() {
                                screen[y as usize] = cropped;
                            }
                        }
                    }
                    break;
                }
                if !image_line.is_empty() {
                    break;
                }
                image_row -= 1;
            }
        }
    }

    paint_scrollbar(boxed, screen, total_width);
}

/// senpi's `renderLayoutFrame`. The `requestRender` callback senpi threads through to
/// `ScrollLayoutState.updateLayout` is unused here: [`ScrollView::update_layout`] (todo 7)
/// returns whether a render is needed instead of invoking a callback, matching this crate's
/// poll-driven render-request pattern (see `tui.rs`'s module doc).
pub fn render_layout_frame(root: &Rc<RefCell<dyn Component>>, width: usize, height: usize) -> LayoutFrame {
    let safe_width = width.max(1);
    let safe_height = height.max(1);
    let mut context = LayoutContext {
        viewport: LayoutViewport {
            width: safe_width,
            height: safe_height,
        },
        render_cache: HashMap::new(),
        primary_scroll_view: None,
    };
    let viewport_rect = LayoutRect {
        x: 0,
        y: 0,
        width: safe_width,
        height: safe_height,
    };
    let root_box = layout_component(&mut context, root, 0, 0, safe_width, Some(safe_height), viewport_rect);
    let mut lines = vec![String::new(); safe_height];
    paint_box(&root_box, &mut lines, safe_width);
    LayoutFrame {
        primary_scroll_view: context.primary_scroll_view,
        root: root_box,
        width: safe_width,
        height: safe_height,
        lines,
    }
}

/// senpi's `getLayoutBoxesAt`: the visual hit path from deepest component to layout root.
pub fn get_layout_boxes_at(frame: &LayoutFrame, x: i64, y: i64) -> Vec<&LayoutBox> {
    let mut result: Vec<(&LayoutBox, i64, i64)> = Vec::new();
    fn visit<'a>(boxed: &'a LayoutBox, x: i64, y: i64, depth: i64, out: &mut Vec<(&'a LayoutBox, i64, i64)>) {
        if !contains_point(boxed.clip, x, y) {
            return;
        }
        out.push((boxed, boxed.layer, depth));
        for child in &boxed.children {
            visit(child, x, y, depth + 1, out);
        }
    }
    visit(&frame.root, x, y, 0, &mut result);
    result.sort_by(|a, b| b.1.cmp(&a.1).then(b.2.cmp(&a.2)));
    result.into_iter().map(|(b, _, _)| b).collect()
}

/// senpi's `getScrollViewBox`.
pub fn get_scroll_view_box<'a>(frame: &'a LayoutFrame, scroll_view: &Rc<RefCell<ScrollView>>) -> Option<&'a LayoutBox> {
    fn visit<'a>(boxed: &'a LayoutBox, target: &Rc<RefCell<ScrollView>>) -> Option<&'a LayoutBox> {
        if let Some(sv) = &boxed.scroll_view
            && Rc::ptr_eq(sv, target)
        {
            return Some(boxed);
        }
        for child in &boxed.children {
            if let Some(found) = visit(child, target) {
                return Some(found);
            }
        }
        None
    }
    visit(&frame.root, scroll_view)
}

/// senpi's `getScrollViewsAt`.
pub fn get_scroll_views_at(frame: &LayoutFrame, x: i64, y: i64) -> Vec<Rc<RefCell<ScrollView>>> {
    let mut result: Vec<(Rc<RefCell<ScrollView>>, i64)> = Vec::new();
    fn visit(boxed: &LayoutBox, x: i64, y: i64, depth: i64, out: &mut Vec<(Rc<RefCell<ScrollView>>, i64)>) {
        if !contains_point(boxed.clip, x, y) {
            return;
        }
        if let Some(sv) = &boxed.scroll_view
            && contains_point(boxed.rect, x, y)
        {
            out.push((Rc::clone(sv), depth));
        }
        for child in &boxed.children {
            visit(child, x, y, depth + 1, out);
        }
    }
    visit(&frame.root, x, y, 0, &mut result);
    result.sort_by_key(|(_, depth)| -*depth);
    result.into_iter().map(|(sv, _)| sv).collect()
}
