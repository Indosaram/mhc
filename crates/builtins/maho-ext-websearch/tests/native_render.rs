use maho_ext_api::{AgentToolResult,ToolRenderContext,ToolRendererSession};
use serde_json::json;

#[test]
fn native_factory_installs_search_renderers() {
    let directory=tempfile::tempdir().unwrap();
    let loaded=maho_ext_host::loader::load_extensions(vec![maho_ext_host::loader::NativeExtensionFactory {
        path:directory.path().join("websearch").to_string_lossy().into_owned(),source_info:Default::default(),
        extension:Box::new(maho_ext_websearch::index::WebsearchExtension {home:directory.path().into(),provider_native_bypass:std::sync::Arc::new(|_|false)}),
    }],directory.path(),Default::default());
    assert!(loaded.errors.is_empty());
    assert!(loaded.extensions[0].tool_renderers.contains_key("web_search"));
}

#[test]
fn search_callbacks_preserve_route_progress_filters_and_expansion() {
    let theme=maho_ext_api::Theme {colors:[("toolTitle","\x1b[38;2;255;255;255m"),("accent","\x1b[38;2;68;170;255m"),("muted","\x1b[38;2;170;170;170m"),("warning","\x1b[38;2;255;170;0m"),("success","\x1b[38;2;0;255;0m"),("error","\x1b[38;2;255;0;0m")].map(|(key,value)|(key.into(),value.into())).into(),..Default::default()};
    for width in [40,80,120] {
        let context=ToolRenderContext {args:json!({"query":"native Rust renderers","allowed_domains":["example.test"]}),tool_call_id:"search-render".into(),invalidate:std::rc::Rc::new(||{}),last_component:None,state:(),cwd:Default::default(),execution_started:false,args_complete:true,is_partial:true,expanded:true,show_images:false,image_protocol:None,is_error:false,has_result:None,spinner_frame:None};
        let mut slots=ToolRendererSession {renderers:std::sync::Arc::new(maho_ext_websearch::websearch::renderers::renderers()),context}.into_slots();
        let mut states=vec![slots.render_call(&theme,width).unwrap()];
        let mut result=AgentToolResult::text("searching");
        result.details=json!({"phase":"searching","query":"native Rust renderers","providerLabels":["exa","brave"],"currentProvider":"brave","attempts":[{"provider":"exa","error":"failed","resultsCount":0}]});
        states.push(slots.render_result(&result,&theme,width).unwrap());
        assert!(states.last().unwrap().join("\n").contains("failed"));
        slots.session.context.is_partial=false;
        slots.session.context.expanded=false;
        result.details=json!({"provider":"brave","durationMs":1500,"truncated":true,"results":(1..=5).map(|i|json!({"title":format!("Source {i}"),"url":format!("https://example.test/{i}"),"snippet":"Relevant native evidence"})).collect::<Vec<_>>()});
        states.push(slots.render_result(&result,&theme,width).unwrap());
        assert!(!states.last().unwrap().join("\n").contains("Source 5"));
        slots.session.context.expanded=true;
        states.push(slots.render_result(&result,&theme,width).unwrap());
        assert!(states.last().unwrap().join("\n").contains("Source 5"));
        result.details=json!({"phase":"error","error":"provider unavailable"});
        states.push(slots.render_result(&result,&theme,width).unwrap());
        for lines in &states {assert!(!lines.is_empty());assert!(lines.iter().all(|line|maho_tui::utils::visible_width(line)<=width));}
        println!("WEBSEARCH_RENDER_JSON={}",json!({"width":width,"states":states}));
    }
}
