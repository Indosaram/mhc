use maho_ext_api::{ExtensionSessionProfile, SourceInfo};
use maho_ext_host::loader::{NativeExtensionFactory, load_extensions};

#[test]
fn native_factory_registers_webfetch_renderers() {
    let directory = tempfile::tempdir().unwrap();
    let loaded = load_extensions(vec![NativeExtensionFactory {
        path: "builtin:webfetch".into(),
        source_info: SourceInfo { source: "builtin".into(), ..Default::default() },
        extension: Box::new(maho_ext_webfetch::index::WebfetchExtension),
    }], directory.path(), ExtensionSessionProfile::default());
    assert!(loaded.errors.is_empty());
    assert!(loaded.extensions[0].tool_renderers.contains_key("webfetch"));
}

#[test]
fn native_callbacks_render_progress_and_results_at_terminal_widths() {
    use maho_ext_api::{AgentToolResult, ToolRenderContext, ToolRendererSession};
    use serde_json::json;
    let directory = tempfile::tempdir().unwrap();
    let theme = maho_ext_api::Theme {
        colors: [("toolTitle", "\x1b[38;2;255;255;255m"), ("accent", "\x1b[38;2;68;170;255m"), ("muted", "\x1b[38;2;170;170;170m"), ("warning", "\x1b[38;2;255;170;0m"), ("success", "\x1b[38;2;0;255;0m")].map(|(key,value)|(key.into(),value.into())).into(),
        ..Default::default()
    };
    for width in [40,80,120] {
        let context = ToolRenderContext {
            args: json!({"url":"https://example.test/article","format":"markdown","timeout":30}),
            tool_call_id:"fetch-render".into(), invalidate:std::rc::Rc::new(||{}), last_component:None,
            state:(), cwd:directory.path().into(), execution_started:false,args_complete:true,is_partial:true,
            expanded:false,show_images:false,image_protocol:None,is_error:false,has_result:None,spinner_frame:None,
        };
        let mut slots = ToolRendererSession {renderers:std::sync::Arc::new(maho_ext_webfetch::webfetch::renderers::renderers()),context}.into_slots();
        let call = slots.render_call(&theme,width).unwrap();
        let mut result = AgentToolResult::text("first line\nsecond line\nthird line\nfourth line\nfifth line");
        let mut states = vec![call];
        for phase in ["fetching","downloading","converting"] {
            result.details=json!({"phase":phase,"url":"https://example.test/article","format":"markdown","timeoutSeconds":30,"bytesRead":1024,"totalBytes":2048});
            states.push(slots.render_result(&result,&theme,width).unwrap());
        }
        slots.session.context.is_partial=false;
        result.details=json!({"status":200,"statusText":"OK","format":"markdown","bytes":2048,"converted":true,"outputTruncated":true,"finalUrl":"https://example.test/article","contentType":"text/html"});
        states.push(slots.render_result(&result,&theme,width).unwrap());
        assert!(!states.last().unwrap().iter().any(|line|line.contains("fifth line")));
        slots.session.context.expanded=true;
        states.push(slots.render_result(&result,&theme,width).unwrap());
        assert!(states.last().unwrap().iter().any(|line|line.contains("fifth line")));
        for lines in &states {assert!(!lines.is_empty());assert!(lines.iter().all(|line|maho_tui::utils::visible_width(line)<=width));}
        println!("WEBFETCH_RENDER_JSON={}",json!({"width":width,"states":states}));
    }
}
