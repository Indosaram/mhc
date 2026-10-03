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
fn registered_callbacks_preserve_exported_foreground_and_background_prefixes() {
    use maho_ext_api::{ToolRenderContext,ToolRendererSession,ToolRenderers};
    use maho_ext_gpt_apply_patch::preview_format::ApplyPatchRenderState;
    use maho_interactive::theme::{Theme,ThemeColor,ThemeBg,theme::ColorMode};
    use serde_json::{Value,json};
    let directory=tempfile::tempdir().unwrap();
    let loaded=load_extensions(vec![NativeExtensionFactory {path:"builtin:gpt-apply-patch".into(),source_info:Default::default(),extension:Box::new(maho_ext_gpt_apply_patch::index::ApplyPatchExtension)}],directory.path(),Default::default());
    let renderers=loaded.extensions[0].tool_renderers["apply_patch"].clone().downcast::<ToolRenderers<ApplyPatchRenderState,Value>>().unwrap();
    for mode in [ColorMode::Color256,ColorMode::Truecolor] {
        let native=Theme::builtin("dark",mode).unwrap();
        // Same export operations as retained lane35's extension_theme.
        let exported=maho_ext_api::Theme {name:Some(native.name.clone()),colors:ThemeColor::ALL.iter().map(|color|(color.key().into(),native.get_fg_ansi(*color))).collect(),backgrounds:ThemeBg::ALL.iter().map(|bg|(bg.key().into(),native.get_bg_ansi(*bg))).collect(),vars:Default::default()};
        let context=ToolRenderContext {args:json!({"input":"*** Begin Patch\n*** Add File: a.txt\n+hello\n"}),tool_call_id:"prefix-proof".into(),invalidate:std::rc::Rc::new(||{}),last_component:None,state:Default::default(),cwd:directory.path().into(),execution_started:false,args_complete:false,is_partial:true,expanded:true,show_images:false,image_protocol:None,is_error:false,has_result:None,spinner_frame:None};
        let mut slots=ToolRendererSession {renderers:renderers.clone(),context}.into_slots();
        let lines=slots.render_call(&exported,80).unwrap().join("\n");
        assert!(lines.contains(&native.get_fg_ansi(ThemeColor::ToolTitle)));
        assert!(lines.contains(&native.get_bg_ansi(ThemeBg::ToolPendingBg)));
        assert!(lines.contains(&native.get_fg_ansi(ThemeColor::ToolDiffAdded)));
        if mode==ColorMode::Color256 {assert!(!lines.contains("\x1b[38;2;"));assert!(!lines.contains("\x1b[48;2;"));}
        println!("PREFIX_RENDER_JSON={}",json!({"mode":format!("{mode:?}"),"lines":lines}));
    }
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
        colors: [("toolTitle", "\x1b[38;2;238;238;238m"), ("toolDiffAdded", "\x1b[38;2;0;255;0m"), ("toolDiffRemoved", "\x1b[38;2;255;0;0m"), ("toolOutput", "\x1b[38;2;238;238;238m"), ("toolDiffContext", "\x1b[38;2;136;136;136m")].map(|(key, value)| (key.into(), value.into())).into(),
        backgrounds: [("toolPendingBg", "\x1b[48;2;32;32;48m"), ("toolSuccessBg", "\x1b[48;2;16;40;16m"), ("toolErrorBg", "\x1b[48;2;48;16;16m")].map(|(key, value)| (key.into(), value.into())).into(),
        ..Default::default()
    };
    for width in [40, 80, 120] {
        let fixture=tempfile::tempdir().unwrap();
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
        let applied = runtime.block_on(maho_ext_gpt_apply_patch::apply::apply_patch_detailed(fixture.path(), "*** Begin Patch\n*** Add File: created.rs\n+let hello = 1;\n*** Update File: missing.rs\n@@\n-old\n+new\n*** End Patch")).unwrap();
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
