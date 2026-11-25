use maho_codemode::tool::{eval_request::parse_eval_request, render::*};
use maho_ext_api::AgentToolResult;
use serde_json::json;

#[test]
fn collapsed_call_keeps_last_four_code_lines() {
    let args = parse_eval_request(
        &json!({"language":"js","summary":" six  line preview ","code":"a\nb\nc\nd\ne\nf"}),
    )
    .unwrap();
    assert_eq!(
        render_eval_call(&args, &Default::default(), 80),
        vec![
            "eval js",
            "six line preview",
            "2 earlier code lines",
            "c",
            "d",
            "e",
            "f"
        ]
    );
    assert!(
        render_eval_call(
            &args,
            &EvalRenderContext {
                has_result: true,
                ..Default::default()
            },
            80
        )
        .is_empty()
    );
}

#[test]
fn result_metadata_and_output_preview_follow_source() {
    let mut result = AgentToolResult::text(
        (1..=10)
            .map(|i| format!("line-{i}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    result.details = json!({"language":"js","durationMs":0,"phase":"summarizing","toolCallCount":2,"wallDurationMs":1000});
    let lines = render_eval_result(
        &result,
        &maho_codemode::tool::types::EvalToolRequest::List,
        &Default::default(),
        80,
    );
    assert_eq!(lines[0], "eval js done");
    assert_eq!(
        lines[1],
        "phase summarizing | took 1s | 2 calls · 2.00 calls/s"
    );
    assert_eq!(lines[3], "2 earlier output lines");
    assert_eq!(lines.last().unwrap(), "line-10");
}

#[test]
fn live_cell_clock_and_reset_badges_are_rendered() {
    let args = parse_eval_request(
        &json!({"language":"js","summary":"work","code":"1+1","reset":true,"timeout":5}),
    )
    .unwrap();
    let mut result = AgentToolResult::text("");
    result.details = json!({"cells":[{"language":"js","code":"1+1","status":"running","startedAt":1000,"output":"2"}]});
    let lines = render_eval_result(
        &result,
        &args,
        &EvalRenderContext {
            now: 4000.0,
            ..Default::default()
        },
        80,
    );
    // Pinned upstream (senpi 903b8336 render-live.ts): fixed-height live frame, headline first, elapsed protected last.
    assert_eq!(
        lines,
        vec![
            "╭─ ⠋ 1+1 · eval js running · reset · timeout 5s · 3s",
            "│ 1+1",
            "│ ",
            "│ ",
            "├─ output",
            "│ 2",
            "│ ",
            "╰─"
        ]
    );
}

#[test]
fn collapsed_calls_bound_error_and_show_exact_omissions() {
    let mut result = AgentToolResult::text("");
    result.details = json!({"language":"js","toolCalls":(0..7).map(|i|json!({"name":format!("call-{i}"),"ok":false,"error":"x".repeat(600)})).collect::<Vec<_>>()});
    let lines = render_eval_result(
        &result,
        &maho_codemode::tool::types::EvalToolRequest::List,
        &Default::default(),
        80,
    );
    assert!(lines.contains(&"2 earlier tool calls".into()));
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.as_str() == "[tool error omitted]")
            .count(),
        5
    );
    assert!(!lines.iter().any(|line| line.contains("tool.call-0")));
}

#[test]
fn registered_renderers_draw_the_eval_card_instead_of_raw_arguments() {
    let renderers = std::sync::Arc::new(eval_tool_renderers());
    let args = json!({"language":"js","summary":"collect project facts","code":"const a = 1;\nconsole.log(a)"});
    let context = maho_ext_api::ToolRenderContext {
        args: args.clone(),
        tool_call_id: "call".into(),
        invalidate: std::rc::Rc::new(|| {}),
        last_component: None,
        state: (),
        cwd: "/tmp".into(),
        execution_started: false,
        args_complete: true,
        is_partial: true,
        expanded: false,
        show_images: false,
        image_protocol: None,
        is_error: false,
        has_result: Some(false),
        spinner_frame: None,
    };
    let mut session = maho_ext_api::ToolRendererSession { renderers, context };
    let lines = session
        .render_call(&Default::default(), 80)
        .expect("eval call renderer");
    assert_eq!(
        lines,
        vec![
            "eval js",
            "collect project facts",
            "const a = 1;",
            "console.log(a)"
        ]
    );
    assert!(!lines.iter().any(|line| line.contains("\"summary\"")));
}

#[test]
fn live_result_card_repaints_itself_each_second_until_terminal() {
    use std::{
        cell::Cell,
        rc::Rc,
        time::{Duration, Instant},
    };
    let renderers = std::sync::Arc::new(eval_tool_renderers());
    let args = json!({"language":"js","summary":"work","code":"1+1"});
    let invalidations = Rc::new(Cell::new(0));
    let counter = invalidations.clone();
    let context = maho_ext_api::ToolRenderContext {
        args,
        tool_call_id: "live".into(),
        invalidate: Rc::new(move || counter.set(counter.get() + 1)),
        last_component: None,
        state: (),
        cwd: "/tmp".into(),
        execution_started: true,
        args_complete: true,
        is_partial: true,
        expanded: false,
        show_images: false,
        image_protocol: None,
        is_error: false,
        has_result: Some(true),
        spinner_frame: None,
    };
    let mut session = maho_ext_api::ToolRendererSession { renderers, context };
    let mut running = AgentToolResult::text("");
    running.details = json!({"cells":[{"language":"js","code":"1+1","status":"running","startedAt":1000,"output":""}]});
    session
        .render_result(&running, &Default::default(), 80)
        .expect("eval result renderer");
    assert!(maho_tui::ui_intervals::run_due_ui_intervals(
        Instant::now() + Duration::from_millis(1_001)
    ));
    assert_eq!(invalidations.get(), 1);
    let mut done = AgentToolResult::text("2");
    done.details = json!({"cells":[{"language":"js","code":"1+1","status":"complete","startedAt":1000,"durationMs":5,"output":"2"}]});
    session.context.is_partial = false;
    session
        .render_result(&done, &Default::default(), 80)
        .expect("eval result renderer");
    assert!(!maho_tui::ui_intervals::run_due_ui_intervals(
        Instant::now() + Duration::from_secs(5)
    ));
    assert_eq!(invalidations.get(), 1);
}

#[test]
fn render_status_env_preserves_non_bmp_unicode() {
    let mut result = AgentToolResult::text("");
    result.details = json!({
        "cells": [{
            "language": "js",
            "code": "1",
            "status": "complete",
            "output": "",
            "statusEvents": [{
                "op": "env",
                "action": "set",
                "key": "ICON",
                "value": "🦀"
            }]
        }]
    });
    let lines = render_eval_result(
        &result,
        &maho_codemode::tool::types::EvalToolRequest::List,
        &EvalRenderContext {
            expanded: true,
            ..Default::default()
        },
        80,
    );
    assert!(lines.iter().any(|line| line.contains("ICON=🦀")));
    assert!(
        !lines
            .iter()
            .any(|line| line.contains("ICON=\u{fffd}\u{fffd}"))
    );
}

/// senpi 903b8336 eval-display-json-bounds.test.ts: legacy stored entries render as bounded placeholders.
#[test]
fn legacy_oversized_display_entries_render_bounded() {
    let render = |value: serde_json::Value, expanded: bool| {
        let mut result = AgentToolResult::text("");
        result.details = json!({"cells":[{"language":"js","code":"1","status":"complete","output":""}],"jsonOutputs":[value]});
        render_eval_result(
            &result,
            &maho_codemode::tool::types::EvalToolRequest::List,
            &EvalRenderContext {
                expanded,
                ..Default::default()
            },
            80,
        )
    };
    let buffer = json!({"type":"Buffer","data":vec![0;3_000_000],"width":1,"height":1});
    let nested = json!({"buffer":{"type":"Buffer","data":vec![0;3_000_000]},"width":1,"height":1});
    for expanded in [false, true] {
        let lines = render(buffer.clone(), expanded);
        assert!(lines.join("\n").contains("<Buffer 2.9 MB>"));
        let lines = render(nested.clone(), expanded);
        let text = lines.join("\n");
        assert!(
            text.contains("buffer: <Buffer 2.9 MB>")
                && text.contains("width: 1")
                && text.contains("height: 1")
        );
        assert!(!text.contains("[0]"));
    }
    let lines = render(json!({"payload":"x".repeat(200_000)}), true);
    assert!(
        lines
            .iter()
            .any(|line| line
                .starts_with("… oversized display value elided ({1 keys}, ~7.8 KB+ stored)")),
        "{lines:?}"
    );
}

#[test]
fn live_eval_card_holds_height_floor_until_terminal_and_resets_on_toggle() {
    let theme = maho_ext_api::Theme {
        colors: [("muted".to_owned(), "<m>".to_owned())].into(),
        ..Default::default()
    };
    let renderers = std::sync::Arc::new(eval_tool_renderers_with(None));
    let args = json!({"language":"js","summary":"work","code":"1+1"});
    let context = maho_ext_api::ToolRenderContext {
        args,
        tool_call_id: "live-floor-1".into(),
        invalidate: std::rc::Rc::new(|| {}),
        last_component: None,
        state: (),
        cwd: "/tmp".into(),
        execution_started: true,
        args_complete: true,
        is_partial: true,
        expanded: false,
        show_images: false,
        image_protocol: None,
        is_error: false,
        has_result: Some(true),
        spinner_frame: Some(0),
    };
    let mut session = maho_ext_api::ToolRendererSession { renderers, context };

    let mut tall = AgentToolResult::text("a\nb\nc\nd\ne");
    tall.details = json!({"cells":[{"language":"js","code":"1+1","status":"running","startedAt":1000,"output":"a\nb\nc\nd\ne"}]});
    let tall_lines = session.render_result(&tall, &theme, 80).expect("tall");

    let mut short_live = AgentToolResult::text("a");
    short_live.details = json!({"cells":[{"language":"js","code":"1+1","status":"running","startedAt":1000,"output":"a"}]});
    let padded_lines = session
        .render_result(&short_live, &theme, 80)
        .expect("padded");
    assert_eq!(padded_lines.len(), tall_lines.len());

    session.context.expanded = true;
    let expanded_lines = session
        .render_result(&short_live, &theme, 80)
        .expect("expanded");
    assert!(expanded_lines.len() < tall_lines.len());

    session.context.expanded = false;
    session.context.is_partial = false;
    let mut done = AgentToolResult::text("a");
    done.details = json!({"cells":[{"language":"js","code":"1+1","status":"complete","startedAt":1000,"durationMs":5,"output":"a"}]});
    let done_lines = session.render_result(&done, &theme, 80).expect("done");
    assert!(done_lines.len() < tall_lines.len());
}

#[test]
fn custom_nested_tool_renderers_draw_call_and_result_and_keep_state() {
    use maho_codemode::tool::tool_widgets::{
        NestedToolRendererLookup, WidgetOptions, is_core_widget_name,
        render_tool_call_widget_with_renderers,
    };
    use maho_ext_api::{ToolRenderers, erase_tool_renderers};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    assert!(is_core_widget_name("bash"));
    assert!(!is_core_widget_name("thread_list"));

    let prefetches = Arc::new(AtomicUsize::new(0));
    let prefetches_in_call = Arc::clone(&prefetches);

    let typed: Arc<ToolRenderers<bool, serde_json::Value>> = Arc::new(ToolRenderers {
        render_call: Some(Arc::new(move |args, _theme, context| {
            if !context.state {
                context.state = true;
                prefetches_in_call.fetch_add(1, Ordering::SeqCst);
            }
            let suffix = if args["all_scope"] == true {
                " all workspaces"
            } else {
                ""
            };
            Box::new(maho_tui::components::text::Text::new(format!(
                "thread_list{suffix}"
            )))
        })),
        render_result: Some(Arc::new(|result, _options, _theme, _context| {
            let count = result.details["result"]["threads"]
                .as_array()
                .map_or(0, Vec::len);
            Box::new(maho_tui::components::text::Text::new(format!(
                "{count} threads in this workspace"
            )))
        })),
    });

    let erased = erase_tool_renderers(typed, "/tmp".into());
    let lookup: NestedToolRendererLookup = Arc::new(move |name| {
        if name == "thread_list" {
            Some(Arc::clone(&erased))
        } else {
            None
        }
    });
    let theme = maho_ext_api::Theme {
        colors: [
            ("success".to_owned(), String::new()),
            ("error".to_owned(), String::new()),
            ("muted".to_owned(), String::new()),
        ]
        .into(),
        ..Default::default()
    };
    let options = WidgetOptions {
        cwd: std::path::Path::new("/tmp"),
        theme: Some(&theme),
        expanded: false,
        width: 80,
    };

    let summary_with_details = json!({
        "name": "thread_list",
        "ok": true,
        "callId": "t-1",
        "args": {"all_scope": true},
        "durationMs": 300,
        "resultPreview": "{\"kind\":\"ok\"}",
        "details": {"result": {"threads": [{"name": "planner"}, {"name": "builder"}]}},
    });
    for _ in 0..3 {
        let lines =
            render_tool_call_widget_with_renderers(&summary_with_details, &options, Some(&lookup));
        let text = lines.join("\n");
        assert!(text.contains("thread_list all workspaces"), "{text}");
        assert!(text.contains("2 threads in this workspace"), "{text}");
    }
    assert_eq!(prefetches.load(Ordering::SeqCst), 1);

    let without_details = json!({
        "name": "thread_list",
        "ok": true,
        "args": {},
        "resultPreview": "{\"kind\":\"ok\"}",
    });
    let fallback_preview =
        render_tool_call_widget_with_renderers(&without_details, &options, Some(&lookup))
            .join("\n");
    assert!(fallback_preview.contains("thread_list"));
    assert!(fallback_preview.contains("{\"kind\":\"ok\"}"));
}
