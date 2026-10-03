use maho_ext_api::{ExtensionSessionProfile, SourceInfo};
use maho_ext_host::loader::{load_extensions, NativeExtensionFactory};

#[test]
fn actual_native_factory_registers_call_and_result_renderers() {
    let directory = tempfile::tempdir().unwrap();
    let loaded = load_extensions(vec![NativeExtensionFactory {
        path: "builtin:gpt-apply-patch".into(),
        source_info: SourceInfo { source: "builtin".into(), ..Default::default() },
        extension: Box::new(maho_ext_gpt_apply_patch::index::ApplyPatchExtension),
    }], directory.path(), ExtensionSessionProfile::default());
    assert!(loaded.errors.is_empty());
    assert!(loaded.extensions[0].tool_renderers.contains_key("apply_patch"));
}

#[test]
fn registered_renderer_session_draws_stream_progress_success_and_failure_at_terminal_widths() {
    use maho_ext_api::{AgentToolResult, ToolRenderContext, ToolRendererSession, ToolRenderers};
    use maho_ext_gpt_apply_patch::preview_format::ApplyPatchRenderState;
    use serde_json::{Value, json};
    let directory = tempfile::tempdir().unwrap();
    let loaded = load_extensions(vec![NativeExtensionFactory {
        path: "builtin:gpt-apply-patch".into(), source_info: SourceInfo { source: "builtin".into(), ..Default::default() },
        extension: Box::new(maho_ext_gpt_apply_patch::index::ApplyPatchExtension),
    }], directory.path(), ExtensionSessionProfile::default());
    let renderers = loaded.extensions[0].tool_renderers["apply_patch"].clone().downcast::<ToolRenderers<ApplyPatchRenderState, Value>>().unwrap();
    let theme = maho_ext_api::Theme {
        colors: [("toolTitle", "#eeeeee"), ("toolDiffAdded", "#00ff00"), ("toolDiffRemoved", "#ff0000"), ("toolOutput", "#eeeeee"), ("toolDiffContext", "#888888")].map(|(key, value)| (key.into(), value.into())).into(),
        backgrounds: [("toolPendingBg", "#202030"), ("toolSuccessBg", "#102810"), ("toolErrorBg", "#301010")].map(|(key, value)| (key.into(), value.into())).into(),
        ..Default::default()
    };
    for width in [40, 80, 120] {
        let context = ToolRenderContext {
            args: json!({"input":"*** Begin Patch\n*** Add File: created.rs\n+let hello = 1;\n"}),
            tool_call_id: "render-proof".into(), invalidate: std::rc::Rc::new(|| {}), last_component: None,
            state: ApplyPatchRenderState::default(), cwd: directory.path().into(), execution_started: false,
            args_complete: false, is_partial: true, expanded: true, show_images: false, image_protocol: None,
            is_error: false, has_result: None, spinner_frame: None,
        };
        let mut session = ToolRendererSession { renderers: renderers.clone(), context }.into_slots();
        let stream = session.render_call(&theme, width).unwrap();
        assert!(!stream.is_empty());
        let mut result = AgentToolResult::text("written");
        result.details = json!({"preview":{"files":[{"filePath":"created.rs","operation":"add","diff":"+1 let hello = 1;","added":1,"removed":0}],"added":1,"removed":0},"progress":{"applied":1,"failed":0,"total":1}});
        let pending = session.render_result(&result, &theme, width).unwrap();
        session.session.context.is_partial = false;
        let success = session.render_result(&result, &theme, width).unwrap();
        session.session.context.is_error = true;
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let applied = runtime.block_on(maho_ext_gpt_apply_patch::apply::apply_patch_detailed(directory.path(), "*** Begin Patch\n*** Add File: created.rs\n+let hello = 1;\n*** Update File: missing.rs\n@@\n-old\n+new\n*** End Patch")).unwrap();
        let (text, details) = maho_ext_gpt_apply_patch::tool::execution_result(applied);
        result = AgentToolResult::text(text);
        result.details = serde_json::to_value(details).unwrap();
        let failure = session.render_result(&result, &theme, width).unwrap();
        assert!(failure.iter().any(|line| line.contains("\u{1b}[")));
        for lines in [&stream, &pending, &success, &failure] {
            assert!(!lines.is_empty());
            assert!(lines.iter().all(|line| maho_tui::utils::visible_width(line) <= width));
        }
        println!("RENDER_WIDTH={width}\n{}\n{}\n{}\n{}", stream.join("\n"), pending.join("\n"), success.join("\n"), failure.join("\n"));
        println!("RENDER_JSON={}", json!({"width":width,"stream":stream,"pending":pending,"success":success,"failure":failure}));
    }
}
